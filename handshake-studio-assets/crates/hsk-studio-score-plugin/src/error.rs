use crate::types::LifecycleState;
use hsk_studio_score::ScoreError;

/// Why a framed message was refused (stable, content-free).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolFault {
    /// Magic bytes absent: the peer is not speaking this protocol.
    Garbage,
    /// A declared frame length above the cap (nothing was allocated or read).
    Oversize,
    /// The stream ended inside a frame.
    Truncated,
    /// Header protocol version differs from this build.
    VersionSkew,
    /// Message kind unknown, or valid but not the reply this request allows.
    UnexpectedKind,
    /// Reply request id differs from the request, or a field is inconsistent.
    Inconsistent,
}

impl ProtocolFault {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Garbage => "garbage",
            Self::Oversize => "oversize",
            Self::Truncated => "truncated",
            Self::VersionSkew => "version_skew",
            Self::UnexpectedKind => "unexpected_kind",
            Self::Inconsistent => "inconsistent",
        }
    }
}

/// Typed, content-free outcome vocabulary of the plugin provider. `code()` strings are wire-stable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginError {
    /// No grant, or the grant names other content than the approved plugin.
    Unauthorized,
    /// The file at the approved path does not hash to the approved SHA-256.
    HashMismatch,
    /// Format, architecture or ABI version is not one this host or worker supports.
    AbiMismatch,
    /// The worker did not answer probe or activation in time; it was killed and reaped.
    ProbeTimeout,
    /// The worker ended without a reply (exit, abort, external kill); it was reaped.
    Crashed { exit_code: Option<i32> },
    /// A call exceeded its deadline; the worker was killed and reaped.
    Timeout,
    /// The block was prepared under a state version that is no longer current.
    StaleState,
    /// Worker count, in-flight bytes, frame size, state size or block capacity bound reached.
    BudgetExceeded,
    /// Cancellation was observed; an in-flight worker call was killed; nothing was published.
    Canceled,
    /// The worker broke the framed protocol; it was killed and reaped.
    Protocol(ProtocolFault),
    /// The lifecycle does not allow this call in its current state.
    IllegalTransition {
        from: LifecycleState,
        to: LifecycleState,
    },
    /// The worker program or the approved plugin file is missing or unreadable, or spawn failed.
    Unavailable,
    /// A configuration value (rate, layout, limits, deadline, parameter) is outside its bound.
    InvalidConfig,
    /// The block handed in does not satisfy the score block contract or the instance configuration.
    InvalidBlock(ScoreError),
    /// The worker returned a non-finite sample; the block was replaced by silence.
    NonFiniteOutput,
}

impl PluginError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::HashMismatch => "hash_mismatch",
            Self::AbiMismatch => "abi_mismatch",
            Self::ProbeTimeout => "probe_timeout",
            Self::Crashed { .. } => "crashed",
            Self::Timeout => "timeout",
            Self::StaleState => "stale_state",
            Self::BudgetExceeded => "budget_exceeded",
            Self::Canceled => "canceled",
            Self::Protocol(_) => "protocol",
            Self::IllegalTransition { .. } => "illegal_transition",
            Self::Unavailable => "unavailable",
            Self::InvalidConfig => "invalid_config",
            Self::InvalidBlock(_) => "invalid_block",
            Self::NonFiniteOutput => "non_finite_output",
        }
    }
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for PluginError {}
