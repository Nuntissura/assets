use core::fmt;

/// Bounded resource whose limit was violated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LimitKind {
    Identifier,
    Text,
    Fields,
    Frame,
    OpaqueValue,
    SchemaSet,
    Loops,
}

/// Typed sequence failure with no unbounded attacker-controlled strings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SequenceFault {
    InvalidStart,
    Duplicate,
    Reordered,
    Gap,
    IdentityChanged,
    DurableAheadOfAdmitted,
    DurableReordered,
    ReplayOutsideWindow,
    Exhausted,
}

/// Validation and transition failures for the diagnostic contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContractError {
    Empty {
        field: &'static str,
    },
    LimitExceeded {
        kind: LimitKind,
        limit: usize,
        actual: usize,
    },
    InvalidIdentifier {
        field: &'static str,
    },
    InvalidSchemaHash,
    InvalidRange,
    IncompatibleMajor,
    NoCompatibleMinor,
    SchemaIncompatible,
    UnknownMandatoryKind,
    IllegalEventPolicy,
    PrivacyViolation,
    DuplicateField,
    DuplicateSchema,
    Sequence {
        fault: SequenceFault,
    },
    InvalidTransition,
    MissingReadyEvidence,
    CounterInvariant,
    PartialFrame,
    MalformedFrame,
    RetentionTerminal,
}

impl fmt::Display for ContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "diagnostic contract violation: {self:?}")
    }
}

impl std::error::Error for ContractError {}
