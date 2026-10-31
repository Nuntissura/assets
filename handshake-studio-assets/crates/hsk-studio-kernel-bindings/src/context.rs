//! The one adapter between the wire and accord's typed command context (lane SMB-ATTRIBUTION-1,
//! RF-06). An [`crate::Envelope`] may carry `command`: the command id, client session, base
//! revision, correlation and causation of the mutating command the exchange belongs to. The
//! envelope's `actor` is the command's actor, so the wire form does not repeat it.
//!
//! `Envelope.correlation_id` is NOT the command correlation: it is the adapter's per-exchange
//! reply-matching counter (a reply is accepted only when it echoes it). The command correlation is
//! the separate typed field `command.correlation` and never matches a reply (test
//! `reply_matching_uses_the_exchange_id_not_the_command_correlation`).
#![deny(clippy::arithmetic_side_effects)]
use crate::wire::{ActorWire, WireError};
use hsk_studio_accord::{
    CausationId, ClientSessionId, CommandContext, CommandId, CorrelationId,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandContextWire {
    /// Registered command id, `null` for an operation without one.
    pub command_id: Option<String>,
    pub client_session: String,
    pub base_revision: u64,
    pub correlation: u64,
    pub causation: Option<u64>,
}

impl CommandContextWire {
    /// The wire form of `context`; its actor travels as the envelope's `actor`.
    pub fn from_context(context: &CommandContext) -> Self {
        Self {
            command_id: context.command().map(|c| c.as_str().to_owned()),
            client_session: context.client_session().as_str().to_owned(),
            base_revision: context.base_revision(),
            correlation: context.correlation().value(),
            causation: context.causation().map(CausationId::value),
        }
    }

    /// The typed context under the envelope's `actor`; `invalid_command_context` when the client
    /// session or command id breaks accord's bounds, `invalid_actor` when the actor does.
    pub fn to_context(&self, actor: &ActorWire) -> Result<CommandContext, WireError> {
        let actor = actor.to_context()?;
        let client_session = ClientSessionId::parse(&self.client_session)
            .map_err(|_| WireError::InvalidCommandContext)?;
        let command = self
            .command_id
            .as_deref()
            .map(CommandId::parse)
            .transpose()
            .map_err(|_| WireError::InvalidCommandContext)?;
        Ok(CommandContext::new(
            actor,
            client_session,
            command,
            self.base_revision,
            CorrelationId::new(self.correlation),
            self.causation.map(CausationId::new),
        ))
    }
}
