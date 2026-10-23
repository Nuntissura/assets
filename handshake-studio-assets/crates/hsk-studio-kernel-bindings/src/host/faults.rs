//! Fault injection for the transient host (V4-04). This whole module is compiled only under
//! `cfg(test)` or the non-default `test-faults` feature (the gate sits on `mod faults` in
//! `host.rs`): a default build has no public item that lets a caller force a failure mode.
//! Proven by `tests/bridge_contract.rs` `fault_hooks_exist_only_under_the_test_faults_feature`
//! and the compile_fail doctests on `FAULT_HOOKS_ARE_NOT_PUBLIC` in `lib.rs`.
use super::{GrantKey, State, TransientHost};

/// One-shot test faults, consumed by the next commit request in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The commit is applied, but the reply is lost (the caller sees no reply at all).
    DropAck,
    /// The principal's grant is revoked after the grant check and before the commit.
    RevokeBeforeCommit,
    /// The resource's lease epoch moves on before the commit.
    BumpEpoch,
}

impl TransientHost {
    /// Queues a fault for the next commit request (FIFO, one fault per commit).
    pub fn plan_fault(&self, fault: Fault) {
        self.lock().faults.push_back(fault);
    }
}

/// Applies the next planned fault, if any, before the commit's own checks. Returns whether the
/// reply to this commit is to be lost.
pub(super) fn before_commit(state: &mut State, edit_key: &GrantKey, resource: &str) -> bool {
    let fault = state.faults.pop_front();
    if fault == Some(Fault::RevokeBeforeCommit) {
        if state.grants.remove(edit_key) {
            state.revoked.insert(edit_key.clone());
        }
        state.tokens.remove(edit_key);
    }
    if fault == Some(Fault::BumpEpoch)
        && let Some(r) = state.resources.get_mut(resource)
    {
        r.epoch = r.epoch.saturating_add(1);
    }
    fault == Some(Fault::DropAck)
}
