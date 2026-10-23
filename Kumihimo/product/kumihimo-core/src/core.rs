use crate::{
    Change, Counter, Envelope, Error, LedgerStatus, Limits, OperationId, Outcome, Response,
    Snapshot,
};
use std::collections::VecDeque;

pub struct Core {
    pub(crate) model: Snapshot,
    pub(crate) next_entity: u64,
    high_watermark: OperationId,
    receipt_capacity: usize,
    receipts: VecDeque<(Envelope, Outcome)>,
}
impl Core {
    pub fn new(
        identity: crate::Identity,
        limits: Limits,
        receipt_capacity: usize,
    ) -> Result<Self, Error> {
        identity.validate()?;
        Ok(Self {
            model: Snapshot::new(identity, limits),
            next_entity: 1,
            high_watermark: OperationId::new(0),
            receipt_capacity,
            receipts: VecDeque::new(),
        })
    }
    pub fn snapshot(&self) -> &Snapshot {
        &self.model
    }
    pub fn ledger_status(&self) -> LedgerStatus {
        LedgerStatus {
            high_watermark: self.high_watermark,
            capacity: self.receipt_capacity,
            retained_operations: self.receipts.iter().map(|(e, _)| e.operation).collect(),
        }
    }
    /// Exclusive mutable access serializes mutations. Hosts coordinate sequence allocation across actors.
    pub fn execute(&mut self, envelope: Envelope) -> Response {
        if envelope.identity != self.model.identity {
            return self.response(&envelope, Err(Error::IdentityMismatch));
        }
        let token_identity = match &envelope.command {
            crate::Command::CommitPreview(token) => Some(token.identity()),
            crate::Command::Restore(token) => Some(token.identity()),
            _ => None,
        };
        if token_identity.is_some_and(|identity| identity != &self.model.identity) {
            return self.response(&envelope, Err(Error::IdentityMismatch));
        }
        if let Some((original, outcome)) = self
            .receipts
            .iter()
            .find(|(e, _)| e.operation == envelope.operation)
        {
            return if original == &envelope {
                Response {
                    outcome: outcome.clone(),
                    replayed: true,
                }
            } else {
                self.response(&envelope, Err(Error::IntentMismatch(envelope.operation)))
            };
        }
        if envelope.operation.sequence() == 0 {
            return self.response(&envelope, Err(Error::ZeroOperation));
        }
        if envelope.operation <= self.high_watermark {
            return self.response(&envelope, Err(Error::ExpiredOperation(envelope.operation)));
        }
        self.high_watermark = envelope.operation;
        let result = if envelope.expected_generation != self.model.generation {
            Err(Error::StaleGeneration {
                expected: envelope.expected_generation,
                actual: self.model.generation,
            })
        } else {
            self.mutate(envelope.command.clone())
        };
        let response = self.response(&envelope, result);
        if self.receipt_capacity > 0 {
            if self.receipts.len() == self.receipt_capacity {
                self.receipts.pop_front();
            }
            self.receipts
                .push_back((envelope, response.outcome.clone()));
        }
        response
    }
    fn response(&self, e: &Envelope, result: Result<Change, Error>) -> Response {
        Response {
            outcome: Outcome {
                identity: self.model.identity.clone(),
                actor: e.actor.clone(),
                operation: e.operation,
                generation: self.model.generation,
                result,
            },
            replayed: false,
        }
    }
    pub(crate) fn next_generation(&self) -> Result<u64, Error> {
        self.model
            .generation
            .checked_add(1)
            .ok_or(Error::Exhausted(Counter::Generation))
    }
    pub(crate) fn allocation(&self) -> Result<(u64, u64), Error> {
        self.next_entity
            .checked_add(1)
            .map(|next| (self.next_entity, next))
            .ok_or(Error::Exhausted(Counter::EntityId))
    }
}

#[cfg(test)]
mod overflow_tests {
    use super::*;
    use crate::Command;
    #[test]
    fn exhaustion_is_atomic() {
        for counter in [Counter::EntityId, Counter::Generation] {
            let mut core = Core::new(
                crate::Identity::new("overflow", format!("{counter:?}")).unwrap(),
                Limits::default(),
                2,
            )
            .unwrap();
            if counter == Counter::EntityId {
                core.next_entity = u64::MAX;
            } else {
                core.model.generation = u64::MAX;
            }
            let before = core.snapshot().clone();
            let next = core.next_entity;
            let r = core.execute(Envelope {
                identity: core.snapshot().identity.clone(),
                actor: "overflow".into(),
                operation: OperationId::new(1),
                expected_generation: before.generation,
                command: Command::CreateWorkspace { owner: None },
            });
            assert_eq!(r.outcome.result, Err(Error::Exhausted(counter)));
            assert_eq!(core.snapshot(), &before);
            assert_eq!(core.next_entity, next);
        }
        assert_eq!(OperationId::new(u64::MAX).checked_next(), None);
    }
}
