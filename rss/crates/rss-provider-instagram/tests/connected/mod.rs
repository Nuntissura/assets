use super::*;

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
