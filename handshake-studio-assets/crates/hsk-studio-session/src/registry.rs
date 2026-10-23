//! Instance-owned, enumerable provider registry: no global state, no life-before-main
//! registration, no discovery by linking. A provider proposes a chronicle-prepared transaction
//! from an immutable snapshot; only the session decides whether it commits.
use crate::error::{ProviderError, RegistryError};
use hsk_studio_accord::CancellationToken;
use hsk_studio_chronicle::{self as chronicle, Prepared, RevisionEntry};
use hsk_studio_folio::{Resolver, Snapshot};
use std::{collections::BTreeMap, fmt};

/// Id of the built-in provider: `params` is a chronicle `Patch` as JSON.
pub const CHRONICLE_PROVIDER_ID: &str = "chronicle.patch";
pub const MAX_PROVIDERS: usize = 64;
pub const MAX_PROVIDER_ID_BYTES: usize = 64;

/// Everything a provider may read: an immutable snapshot, the property revision vector, the
/// caller's opaque parameters, the budget, the folio resolver and the cancellation token.
pub struct ProposalRequest<'a> {
    pub snapshot: &'a Snapshot,
    pub revisions: &'a [RevisionEntry],
    pub params: &'a [u8],
    pub budget: chronicle::Budget,
    pub resolver: &'a dyn Resolver,
    pub cancel: &'a CancellationToken,
}

/// Computes a transaction against an immutable snapshot. Providers hold no document state and
/// cannot publish: the session re-checks document, revision and property revisions before it
/// commits what a provider proposed.
pub trait ProposalProvider: Send + Sync {
    /// One-line, content-free description for enumeration.
    fn summary(&self) -> &'static str {
        ""
    }
    fn propose(&self, request: &ProposalRequest<'_>) -> Result<Prepared, ProviderError>;
}

/// Decodes a chronicle `Patch` from `params` and prepares it with chronicle.
#[derive(Clone, Copy, Debug, Default)]
pub struct ChronicleProvider;

impl ProposalProvider for ChronicleProvider {
    fn summary(&self) -> &'static str {
        "chronicle source-local property patch (layer/artboard/page-spread name, layer visible)"
    }

    fn propose(&self, request: &ProposalRequest<'_>) -> Result<Prepared, ProviderError> {
        let patch = chronicle::decode_patch(request.params, request.budget, request.cancel)
            .map_err(provider_error)?;
        chronicle::prepare(
            &patch,
            request.snapshot,
            request.revisions,
            request.budget,
            request.cancel,
            request.resolver,
        )
        .map_err(provider_error)
    }
}

/// Maps a chronicle diagnostic onto the provider error vocabulary (cancellation stays distinct).
pub fn provider_error(diagnostic: chronicle::Diagnostic) -> ProviderError {
    use chronicle::Code;
    ProviderError::Rejected(match diagnostic.code {
        Code::Canceled => return ProviderError::Canceled,
        Code::InvalidInput => "invalid_input",
        Code::UnsupportedVersion => "unsupported_version",
        Code::DuplicateField => "duplicate_field",
        Code::InvalidActor => "invalid_actor",
        Code::InvalidCommand => "invalid_command",
        Code::WrongDocument => "wrong_document",
        Code::InvalidTarget => "invalid_target",
        Code::WrongType => "wrong_type",
        Code::DuplicateAddress => "duplicate_address",
        Code::MissingRead => "missing_read",
        Code::RevisionUnavailable => "revision_unavailable",
        Code::RevisionConflict => "revision_conflict",
        Code::ValueConflict => "value_conflict",
        Code::RevisionOverflow => "revision_overflow",
        Code::BudgetExceeded => "budget_exceeded",
        Code::FolioRejected => "folio_rejected",
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderInfo<'a> {
    pub id: &'a str,
    pub summary: &'static str,
}

#[derive(Default)]
pub struct ProviderRegistry {
    entries: BTreeMap<String, Box<dyn ProposalProvider>>,
}

impl ProviderRegistry {
    /// Empty registry: a session built on it can open and save but proposes nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registry holding the built-in chronicle provider under [`CHRONICLE_PROVIDER_ID`].
    pub fn with_builtin() -> Self {
        let mut registry = Self::new();
        registry.entries.insert(
            CHRONICLE_PROVIDER_ID.to_owned(),
            Box::new(ChronicleProvider),
        );
        registry
    }

    pub fn register(
        &mut self,
        id: &str,
        provider: Box<dyn ProposalProvider>,
    ) -> Result<(), RegistryError> {
        let valid = !id.is_empty()
            && id.len() <= MAX_PROVIDER_ID_BYTES
            && id.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
            });
        if !valid {
            return Err(RegistryError::InvalidId);
        }
        if self.entries.contains_key(id) {
            return Err(RegistryError::DuplicateId);
        }
        if self.entries.len() >= MAX_PROVIDERS {
            return Err(RegistryError::Full);
        }
        self.entries.insert(id.to_owned(), provider);
        Ok(())
    }

    pub fn contains(&self, id: &str) -> bool {
        self.entries.contains_key(id)
    }

    pub fn get(&self, id: &str) -> Option<&dyn ProposalProvider> {
        self.entries.get(id).map(|provider| &**provider)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Registered ids in sorted order (deterministic enumeration).
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    pub fn list(&self) -> Vec<ProviderInfo<'_>> {
        self.entries
            .iter()
            .map(|(id, provider)| ProviderInfo {
                id,
                summary: provider.summary(),
            })
            .collect()
    }
}

impl fmt::Debug for ProviderRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.ids()).finish()
    }
}
