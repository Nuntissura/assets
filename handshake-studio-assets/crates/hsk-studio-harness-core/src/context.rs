//! The caller's attribution of a [`crate::Run`] (lane SMB-ATTRIBUTION-1, RF-06; GUI-DESKTOP-029,
//! -041, -045, -050, STU-ARC-014/015): the actor and client session of the host session that drives
//! the run, and the host's correlation source. A run mints one accord [`CommandContext`] per
//! mutating command from it; nothing in this crate's product code builds a fixed actor.
//!
//! [`asset_local_test_context`] (non-default feature `asset-local-test-host`) is the asset-local
//! test host's one fixed context: the harness CLI and the tests use it; product hosts pass their
//! own [`RunContext`].
use hsk_studio_accord::{ActorContext, BusinessEventSink, CausationId, ClientSessionId, CommandContext, CommandId, CorrelationSource};
use std::sync::{Arc, Mutex};

/// A shared accord business-event sink (lane SMB-BUSINESS-EVENTS-1): the run locks it for each
/// non-blocking `try_emit`; the caller keeps its own handle to inspect what was delivered.
pub type SharedBusinessEventSink = Arc<Mutex<dyn BusinessEventSink>>;

/// Actor, client session and correlation source of one run (cloning shares the source, so a
/// viewer that rebuilds its run keeps one monotonic correlation sequence).
#[derive(Clone)]
pub struct RunContext {
    actor: ActorContext,
    client_session: ClientSessionId,
    correlation: Arc<dyn CorrelationSource>,
    /// Where the run emits business events after accepted commits (lane SMB-BUSINESS-EVENTS-1);
    /// `None`: no emission, recorded as `no_sink` in the op records.
    business_events: Option<SharedBusinessEventSink>,
}

impl RunContext {
    pub fn new(actor: ActorContext, client_session: ClientSessionId, correlation: Arc<dyn CorrelationSource>) -> Self {
        Self { actor, client_session, correlation, business_events: None }
    }

    /// This context with an accord business-event sink (additive; the host's sink is MT-36230,
    /// the asset-local one is kernel-bindings `TransientBusinessEvents`).
    #[must_use]
    pub fn with_business_events(mut self, sink: SharedBusinessEventSink) -> Self {
        self.business_events = Some(sink);
        self
    }

    /// The business-event sink, when one is configured.
    pub fn business_events(&self) -> Option<&SharedBusinessEventSink> {
        self.business_events.as_ref()
    }

    pub fn actor(&self) -> &ActorContext {
        &self.actor
    }

    pub fn client_session(&self) -> &ClientSessionId {
        &self.client_session
    }

    /// A fresh command context: this run's actor and client session, the next correlation of the
    /// source, `command` (a `RUN_COMMANDS` id; `None` for an operation without one) issued against
    /// `base_revision`, caused by `causation`.
    pub fn command(&self, command: Option<&str>, base_revision: u64, causation: Option<CausationId>) -> CommandContext {
        CommandContext::new(
            self.actor.clone(),
            self.client_session.clone(),
            command.and_then(|c| CommandId::parse(c).ok()),
            base_revision,
            self.correlation.next(),
            causation,
        )
    }
}

impl std::fmt::Debug for RunContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunContext")
            .field("actor", &self.actor)
            .field("client_session", &self.client_session)
            .field("business_events", &self.business_events.is_some())
            .finish_non_exhaustive()
    }
}

/// The asset-local test host's fixed context (feature `asset-local-test-host`): a static actor, the
/// client session `asset-local-test-host` and a fresh in-memory [`hsk_studio_accord::TransientCorrelation`]
/// (correlations 1, 2, ...). The only place a fixed context is built (lane notes: a source grep finds
/// it only in feature-gated asset-local code and the harness).
#[cfg(feature = "asset-local-test-host")]
pub fn asset_local_test_context() -> RunContext {
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").expect("static actor literals satisfy accord bounds");
    let client_session = ClientSessionId::parse("asset-local-test-host").expect("static client session satisfies accord bounds");
    RunContext::new(actor, client_session, Arc::new(hsk_studio_accord::TransientCorrelation::new()))
}
