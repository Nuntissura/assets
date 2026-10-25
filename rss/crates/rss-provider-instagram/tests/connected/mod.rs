use super::*;
mod home;
mod following;

struct AuthorizedFetch {
    inner: Fetch,
    generation: AtomicU64,
    checks: AtomicU64,
    reject: Mutex<Option<(u64, rss_contracts::Error)>>,
    generations: Mutex<Vec<Revision>>,
}
impl AuthorizedFetchPort for AuthorizedFetch {
    fn current<'a>(
        &'a self,
        _: &'a SourceRequest,
        _: &'a HostRef,
        context: &'a AuthorizedContext,
    ) -> Pending<'a, ()> {
        Box::pin(async move {
            let check = self.checks.fetch_add(1, Ordering::SeqCst) + 1;
            if let Some((at, error)) = self.reject.lock().unwrap().as_ref() {
                if check == *at {
                    return Err(error.clone());
                }
            }
            if context.lease_generation.0 != self.generation.load(Ordering::SeqCst) {
                return Err(rss_contracts::Error::StaleResult);
            }
            Ok(())
        })
    }
    fn fetch<'a>(
        &'a self,
        source: &'a SourceRequest,
        execution: &'a HostRef,
        context: &'a AuthorizedContext,
        request: FetchRequest,
    ) -> Pending<'a, FetchResponse> {
        self.generations
            .lock()
            .unwrap()
            .push(context.lease_generation);
        self.inner.fetch(source, execution, request)
    }
}
fn authorized(
    mode: AccessMode,
    replies: Vec<Reply>,
) -> AuthorizedInstagramProvider<AuthorizedFetch, Canonical> {
    let base = provider(mode, replies);
    AuthorizedInstagramProvider {
        authorization: AuthorizedContext {
            connection_ref: reference("connection"),
            owner_scope: base.source.owner_scope.clone(),
            external_identity: "999999".into(),
            lease_generation: Revision(1),
        },
        source: base.source,
        execution_ref: base.execution_ref,
        limits: base.limits,
        supported_extensions: base.supported_extensions,
        fetch: AuthorizedFetch {
            inner: base.fetch,
            generation: AtomicU64::new(1),
            checks: AtomicU64::new(0),
            reject: Mutex::new(None),
            generations: Mutex::new(vec![]),
        },
        canonical: base.canonical,
    }
}
fn selected(mode: AccessMode, operation: u64) -> SourceRequest {
    let mut result = request(mode, operation);
    result.subscription.partition.connection_ref = Some(reference("connection"));
    result
}

#[test]
fn authorized_public_content_does_not_become_the_logged_in_accounts_content() {
    for mode in [AccessMode::PublicProfile, AccessMode::PublicReels] {
        let adapter = authorized(
            mode,
            vec![ok(identity()), ok(page(mode, vec![item(1)], None))],
        );
        let result = run(adapter.acquire(selected(mode, 1))).unwrap();
        assert_eq!(result.partition.mode, mode);
        assert_eq!(
            result.partition.connection_ref,
            Some(reference("connection"))
        );
        assert_eq!(
            result.entries[0].observation.provider.as_deref(),
            Some("instagram")
        );
        let requests = adapter.fetch.inner.requests.lock().unwrap();
        assert!(requests[0].url.ends_with("username=kngemma"));
        assert!(
            requests[1].url.contains("123456")
                || requests[1]
                    .form
                    .contains(&("target_user_id".into(), "123456".into()))
        );
        assert_ne!(adapter.authorization.external_identity, "123456");
        assert!(adapter.capabilities().iter().all(|capability| matches!(
            capability.mode,
            AccessMode::PublicProfile | AccessMode::PublicReels
        )));
    }
}

#[test]
fn anonymous_transport_and_invalid_authorization_reject_before_callbacks() {
    let public = provider(AccessMode::PublicProfile, vec![]);
    assert!(matches!(
        run(public.acquire(selected(AccessMode::PublicProfile, 1))),
        Err(rss_contracts::Error::Denied)
    ));
    assert!(public.fetch.requests.lock().unwrap().is_empty());
    for change in 0..5 {
        let mut adapter = authorized(AccessMode::PublicProfile, vec![]);
        let mut request = selected(AccessMode::PublicProfile, 1);
        match change {
            0 => adapter.authorization.connection_ref = reference("foreign"),
            1 => adapter.authorization.owner_scope = ScopeRef("foreign".into()),
            2 => adapter.authorization.external_identity.clear(),
            3 => adapter.authorization.lease_generation = Revision(0),
            _ => request.subscription.partition.connection_ref = None,
        }
        assert!(run(adapter.acquire(request)).is_err());
        assert_eq!(adapter.fetch.checks.load(Ordering::SeqCst), 0);
        assert!(adapter.fetch.inner.requests.lock().unwrap().is_empty());
        assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn lease_loss_before_or_during_preparation_never_returns_success() {
    for (at, error, fetches, canonical_calls) in [
        (1, rss_contracts::Error::Expired, 0, 0),
        (2, rss_contracts::Error::Denied, 1, 0),
        (5, rss_contracts::Error::Denied, 2, 0),
        (6, rss_contracts::Error::Denied, 2, 1),
    ] {
        let adapter = authorized(
            AccessMode::PublicProfile,
            vec![
                ok(identity()),
                ok(page(AccessMode::PublicProfile, vec![item(1)], None)),
            ],
        );
        *adapter.fetch.reject.lock().unwrap() = Some((at, error.clone()));
        assert_eq!(
            run(adapter.acquire(selected(AccessMode::PublicProfile, 1))),
            Err(error)
        );
        assert_eq!(adapter.fetch.inner.requests.lock().unwrap().len(), fetches);
        assert_eq!(
            adapter.canonical.calls.load(Ordering::SeqCst),
            canonical_calls
        );
        assert_eq!(
            adapter.canonical.identities.lock().unwrap().len(),
            canonical_calls as usize
        );
    }
    let adapter = authorized(AccessMode::PublicProfile, vec![]);
    adapter.fetch.generation.store(2, Ordering::SeqCst);
    assert_eq!(
        run(adapter.acquire(selected(AccessMode::PublicProfile, 1))),
        Err(rss_contracts::Error::StaleResult)
    );
    assert!(adapter.fetch.inner.requests.lock().unwrap().is_empty());
}

#[test]
fn renewal_preserves_exact_continuation_but_login_replacement_does_not() {
    let mode = AccessMode::PublicProfile;
    let data = page(mode, vec![item(1), item(2), item(3)], None);
    let mut adapter = authorized(
        mode,
        vec![ok(identity()), ok(data.clone()), ok(identity()), ok(data)],
    );
    let first = run(adapter.acquire(selected(mode, 1))).unwrap();
    let continuation = first.continuation.unwrap();
    let mut resume = selected(mode, 2);
    resume.continuation = Some(continuation.clone());
    adapter.authorization.lease_generation = Revision(2);
    adapter.fetch.generation.store(2, Ordering::SeqCst);
    let last = run(adapter.acquire(resume.clone())).unwrap();
    assert_eq!(last.entries[0].provider_entry_key, "3");
    assert!(last.continuation.is_none());
    assert_eq!(
        *adapter.fetch.generations.lock().unwrap(),
        vec![Revision(1), Revision(1), Revision(2), Revision(2)]
    );
    assert!(adapter.fetch.inner.requests.lock().unwrap()[3]
        .expected_digest
        .is_some());
    assert!(!continuation.token.contains("lease_generation"));
    let fetch_count = adapter.fetch.inner.requests.lock().unwrap().len();
    adapter.authorization.external_identity = "888888".into();
    assert_eq!(
        run(adapter.acquire(resume)),
        Err(rss_contracts::Error::StaleResult)
    );
    assert_eq!(
        adapter.fetch.inner.requests.lock().unwrap().len(),
        fetch_count
    );
}

fn profile_identity(private: bool, followed: Option<bool>, count: u64) -> Value {
    let mut value = identity();
    let user = &mut value["data"]["user"];
    user["is_private"] = json!(private);
    user["media_count"] = json!(count);
    if let Some(followed) = followed { user["followed_by_viewer"] = json!(followed); }
    value
}
fn connected_profile(mode: AccessMode, replies: Vec<Reply>)
    -> rss_provider_instagram::connected::ConnectedInstagramProvider<AuthorizedFetch, Canonical> {
    let mut base = authorized(AccessMode::PublicProfile, replies);
    if mode == AccessMode::OwnUploads { base.authorization.external_identity = "123456".into(); }
    rss_provider_instagram::connected::ConnectedInstagramProvider {
        source: base.source, execution_ref: base.execution_ref, limits: base.limits,
        supported_extensions: base.supported_extensions, authorization: base.authorization,
        fetch: base.fetch, canonical: base.canonical,
    }
}

#[test]
fn own_and_private_profiles_preserve_selected_modes_and_target_identity() {
    for mode in [AccessMode::OwnUploads, AccessMode::PrivateSource] {
        let adapter = connected_profile(mode, vec![ok(profile_identity(true, Some(true), 1)),
            ok(page(AccessMode::PublicProfile, vec![item(1)], None))]);
        let result = run(adapter.acquire(selected(mode, 1))).unwrap();
        assert_eq!(result.partition, selected(mode, 1).subscription.partition);
        assert_eq!(result.entries[0].provider_entry_key, "1");
        assert_eq!(result.coverage, Coverage::Partial);
        assert_eq!(adapter.fetch.inner.requests.lock().unwrap()[1].url,
            "https://www.instagram.com/api/v1/feed/user/123456/?count=30");
        assert_eq!(adapter.capabilities().iter().map(|entry| entry.mode).collect::<Vec<_>>(),
            vec![AccessMode::OwnUploads, AccessMode::PrivateSource]);
    }
    let mut owner = connected_profile(AccessMode::PrivateSource, vec![ok(profile_identity(true, None, 1)),
        ok(page(AccessMode::PublicProfile, vec![item(1)], None))]);
    owner.authorization.external_identity = "123456".into();
    assert!(run(owner.acquire(selected(AccessMode::PrivateSource, 1))).is_ok());
}

#[test]
fn own_identity_and_private_grants_deny_before_content_or_preparation() {
    for malformed_id in [Value::Null, json!("invalid")] {
        let mut private = profile_identity(true, Some(true), 1);
        private["data"]["user"]["id"] = malformed_id;
        assert!(matches!(normalize::account(&serde_json::to_vec(&private).unwrap(), "kngemma", 100000, &bounds()),
            Err(rss_contracts::Error::Denied)));
    }
    for value in [profile_identity(true, None, 1), profile_identity(true, Some(false), 1),
        profile_identity(false, Some(true), 1)] {
        let adapter = connected_profile(AccessMode::PrivateSource, vec![ok(value)]);
        assert_eq!(run(adapter.acquire(selected(AccessMode::PrivateSource, 1))), Err(rss_contracts::Error::Denied));
        assert_eq!(adapter.fetch.inner.requests.lock().unwrap().len(), 1);
        assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst), 0);
    }
    let mut own = connected_profile(AccessMode::OwnUploads, vec![ok(identity())]);
    own.authorization.external_identity = "999999".into();
    assert_eq!(run(own.acquire(selected(AccessMode::OwnUploads, 1))), Err(rss_contracts::Error::Denied));
    assert_eq!(own.fetch.inner.requests.lock().unwrap().len(), 1);
    assert_eq!(own.canonical.calls.load(Ordering::SeqCst), 0);
    for public_mode in [AccessMode::PublicProfile, AccessMode::PublicReels] {
        let public = authorized(public_mode, vec![ok(profile_identity(true, Some(true), 1))]);
        assert_eq!(run(public.acquire(selected(public_mode, 1))), Err(rss_contracts::Error::Denied));
        assert_eq!(public.canonical.calls.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn connected_profiles_require_exact_connection_and_supported_mode_before_callbacks() {
    for mode in [AccessMode::HomeForYou, AccessMode::Following, AccessMode::PublicProfile] {
        let adapter = connected_profile(mode, vec![]);
        assert!(matches!(run(adapter.acquire(selected(mode, 1))), Err(rss_contracts::Error::Unsupported { .. })));
        assert_eq!(adapter.fetch.checks.load(Ordering::SeqCst), 0);
    }
    for changed in 0..4 {
        let mut adapter = connected_profile(AccessMode::OwnUploads, vec![]);
        let mut request = selected(AccessMode::OwnUploads, 1);
        match changed {
            0 => request.subscription.partition.connection_ref = None,
            1 => adapter.authorization.connection_ref = reference("other"),
            2 => adapter.authorization.owner_scope = ScopeRef("other".into()),
            _ => adapter.authorization.external_identity = "not-numeric".into(),
        }
        assert!(run(adapter.acquire(request)).is_err());
        assert_eq!(adapter.fetch.checks.load(Ordering::SeqCst), 0);
        assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn connected_profile_empty_requires_positive_identity_and_terminal_content() {
    for mode in [AccessMode::OwnUploads, AccessMode::PrivateSource] {
        let adapter = connected_profile(mode, vec![ok(profile_identity(true, Some(true), 0)),
            ok(page(AccessMode::PublicProfile, vec![], None))]);
        let result = run(adapter.acquire(selected(mode, 1))).unwrap();
        assert!(matches!(result.state, rss_contracts::AcquisitionState::VerifiedEmpty { .. }));
        assert_eq!(result.coverage, Coverage::Complete);
        assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst), 0);
        for malformed in [json!({"status":"ok","items":[]}),
            json!({"status":"ok","items":[],"more_available":true,"next_max_id":"next"}),
            json!({"status":"fail","message":"login_required","items":[],"more_available":false})] {
            let adapter = connected_profile(mode, vec![ok(profile_identity(true, Some(true), 0)), ok(malformed)]);
            assert!(run(adapter.acquire(selected(mode, 1))).is_err());
            assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst), 0);
        }
        let ambiguous = connected_profile(mode, vec![ok(profile_identity(true, Some(true), 4)),
            ok(page(AccessMode::PublicProfile, vec![], None))]);
        assert!(run(ambiguous.acquire(selected(mode, 1))).is_err());
    }
}

#[test]
fn connected_profile_continuation_retains_mode_digest_and_login_through_renewal() {
    for mode in [AccessMode::OwnUploads, AccessMode::PrivateSource] {
        let data = page(AccessMode::PublicProfile, vec![item(1), item(2), item(3)], None);
        let identity = profile_identity(true, Some(true), 3);
        let mut adapter = connected_profile(mode, vec![ok(identity.clone()), ok(data.clone()), ok(identity), ok(data)]);
        let continuation = run(adapter.acquire(selected(mode, 1))).unwrap().continuation.unwrap();
        assert_eq!(continuation.partition.mode, mode);
        let cursor: Value = serde_json::from_str(&continuation.token).unwrap();
        assert_eq!(cursor["mode"], serde_json::to_value(mode).unwrap());
        let mut resume = selected(mode, 2);
        resume.continuation = Some(continuation.clone());
        adapter.authorization.lease_generation = Revision(2);
        adapter.fetch.generation.store(2, Ordering::SeqCst);
        let result = run(adapter.acquire(resume.clone())).unwrap();
        assert_eq!(result.entries[0].provider_entry_key, "3");
        assert!(result.continuation.is_none());
        assert!(adapter.fetch.inner.requests.lock().unwrap()[3].expected_digest.is_some());
        assert!(!continuation.token.contains("lease_generation"));
        adapter.authorization.external_identity = "888888".into();
        assert_eq!(run(adapter.acquire(resume)), Err(rss_contracts::Error::StaleResult));
        assert_eq!(adapter.fetch.inner.requests.lock().unwrap().len(), 4);
    }
}

#[test]
fn connected_profile_lease_loss_never_returns_success_or_prepares_after_denial() {
    for mode in [AccessMode::OwnUploads, AccessMode::PrivateSource] {
        for (at, error, fetches, preparations) in [
            (1, rss_contracts::Error::Expired, 0, 0), (2, rss_contracts::Error::Denied, 1, 0),
            (5, rss_contracts::Error::Denied, 2, 0), (6, rss_contracts::Error::Denied, 2, 1)] {
            let adapter = connected_profile(mode, vec![ok(profile_identity(true, Some(true), 1)),
                ok(page(AccessMode::PublicProfile, vec![item(1)], None))]);
            *adapter.fetch.reject.lock().unwrap() = Some((at, error.clone()));
            assert_eq!(run(adapter.acquire(selected(mode, 1))), Err(error));
            assert_eq!(adapter.fetch.inner.requests.lock().unwrap().len(), fetches);
            assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst), preparations);
        }
    }
}

#[test]
fn connected_profile_retained_changes_and_revoked_target_do_not_prepare_more() {
    for mode in [AccessMode::OwnUploads, AccessMode::PrivateSource] {
        for changed in 0..3 {
            let identity = profile_identity(true, Some(true), 3);
            let data = page(AccessMode::PublicProfile, vec![item(1), item(2), item(3)], None);
            let mut next_identity = identity.clone();
            let mut next_data = data.clone();
            let status = match changed {
                0 => { next_data["items"][2]["caption"]["text"] = json!("changed"); 200 },
                1 => 304,
                _ => {
                    if mode == AccessMode::PrivateSource {
                        next_identity["data"]["user"]["followed_by_viewer"] = json!(false);
                    } else { next_identity["data"]["user"]["id"] = json!("888888"); }
                    200
                },
            };
            let adapter = connected_profile(mode, vec![ok(identity), ok(data), ok(next_identity), Reply { value: next_data, status }]);
            let first = run(adapter.acquire(selected(mode, 1))).unwrap();
            assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst), 2);
            let mut resume = selected(mode, 2);
            resume.continuation = first.continuation;
            assert!(run(adapter.acquire(resume)).is_err());
            assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst), 2);
        }
    }
}

#[test]
fn connected_profile_repeated_cursor_and_later_malformed_media_preserve_prior_effects() {
    for mode in [AccessMode::OwnUploads, AccessMode::PrivateSource] {
        let identity = profile_identity(true, Some(true), 3);
        let adapter = connected_profile(mode, vec![ok(identity.clone()),
            ok(page(AccessMode::PublicProfile, vec![item(1)], Some("repeat"))), ok(identity.clone()),
            ok(page(AccessMode::PublicProfile, vec![item(2)], Some("repeat")))]);
        let first = run(adapter.acquire(selected(mode, 1))).unwrap();
        let mut resume = selected(mode, 2);
        resume.continuation = first.continuation;
        assert!(run(adapter.acquire(resume)).is_err());
        assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst), 1);
        let mut malformed = item(3);
        malformed["image_versions2"]["candidates"] = json!([]);
        let adapter = connected_profile(mode, vec![ok(identity),
            ok(page(AccessMode::PublicProfile, vec![item(1), item(2), malformed], None))]);
        assert!(run(adapter.acquire(selected(mode, 1))).is_err());
        assert_eq!(adapter.canonical.calls.load(Ordering::SeqCst), 0);
    }
}
