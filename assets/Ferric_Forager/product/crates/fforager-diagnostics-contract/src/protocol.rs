#[cfg(test)]
use crate::MAX_ID_BYTES;
use crate::{
    BootSessionId, BoundedText, ChannelId, ContractError, FIRST_SEQUENCE, LAST_SEQUENCE, LimitKind,
    MAX_SCHEMA_IDENTITIES, ProducerInstanceId, SequenceFault,
};
use serde::{Deserialize, Serialize};

/// Diagnostic protocol version. Major changes are incompatible; minor versions negotiate.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields, try_from = "ProtocolVersionWire")]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProtocolVersionWire {
    major: u16,
    minor: u16,
}

impl ProtocolVersion {
    /// Creates a version with a nonzero protocol major.
    ///
    /// # Errors
    /// Returns [`ContractError`] for major zero.
    pub fn new(major: u16, minor: u16) -> Result<Self, ContractError> {
        if major == 0 {
            return Err(ContractError::InvalidRange);
        }
        Ok(Self { major, minor })
    }

    /// Revalidates a protocol version assembled by Rust code.
    ///
    /// # Errors
    /// Returns [`ContractError`] for major zero.
    pub fn validate(self) -> Result<(), ContractError> {
        Self::new(self.major, self.minor).map(|_| ())
    }
}

impl TryFrom<ProtocolVersionWire> for ProtocolVersion {
    type Error = ContractError;

    fn try_from(value: ProtocolVersionWire) -> Result<Self, Self::Error> {
        Self::new(value.major, value.minor)
    }
}

/// Inclusive minor-version range supported for one protocol major.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, try_from = "CompatibilityRangeWire")]
pub struct CompatibilityRange {
    pub major: u16,
    pub minimum_minor: u16,
    pub maximum_minor: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompatibilityRangeWire {
    major: u16,
    minimum_minor: u16,
    maximum_minor: u16,
}

impl CompatibilityRange {
    /// Creates an inclusive minor range for a nonzero major version.
    ///
    /// # Errors
    /// Returns [`ContractError`] for major zero or an inverted minor range.
    pub fn new(major: u16, minimum_minor: u16, maximum_minor: u16) -> Result<Self, ContractError> {
        if major == 0 || minimum_minor > maximum_minor {
            return Err(ContractError::InvalidRange);
        }
        Ok(Self {
            major,
            minimum_minor,
            maximum_minor,
        })
    }

    /// Revalidates a compatibility range assembled by Rust code.
    ///
    /// # Errors
    /// Returns [`ContractError`] for major zero or an inverted range.
    pub fn validate(self) -> Result<(), ContractError> {
        Self::new(self.major, self.minimum_minor, self.maximum_minor).map(|_| ())
    }
}

impl TryFrom<CompatibilityRangeWire> for CompatibilityRange {
    type Error = ContractError;

    fn try_from(value: CompatibilityRangeWire) -> Result<Self, Self::Error> {
        Self::new(value.major, value.minimum_minor, value.maximum_minor)
    }
}

/// Canonical schema digest algorithm.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaHashAlgorithm {
    Sha256,
}

/// Lowercase 64-digit SHA-256 digest.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SchemaHash(String);

impl SchemaHash {
    /// Creates a canonical lowercase SHA-256 hexadecimal digest.
    ///
    /// # Errors
    /// Returns [`ContractError`] unless exactly 64 lowercase hexadecimal digits are supplied.
    pub fn new(value: impl Into<String>) -> Result<Self, ContractError> {
        let value = value.into();
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ContractError::InvalidSchemaHash);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for SchemaHash {
    type Error = ContractError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<SchemaHash> for String {
    fn from(value: SchemaHash) -> Self {
        value.0
    }
}

/// Schema identity and the canonical-input revision hashed to produce it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, try_from = "SchemaIdentityWire")]
pub struct SchemaIdentity {
    pub algorithm: SchemaHashAlgorithm,
    pub canonical_input_version: u16,
    pub digest: SchemaHash,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SchemaIdentityWire {
    algorithm: SchemaHashAlgorithm,
    canonical_input_version: u16,
    digest: SchemaHash,
}

impl SchemaIdentity {
    /// Creates a schema identity over a nonzero canonical-input revision.
    ///
    /// # Errors
    /// Returns [`ContractError`] for canonical-input revision zero.
    pub fn new(
        algorithm: SchemaHashAlgorithm,
        canonical_input_version: u16,
        digest: SchemaHash,
    ) -> Result<Self, ContractError> {
        if canonical_input_version == 0 {
            return Err(ContractError::InvalidRange);
        }
        Ok(Self {
            algorithm,
            canonical_input_version,
            digest,
        })
    }

    /// Revalidates a schema identity assembled by Rust code.
    ///
    /// # Errors
    /// Returns [`ContractError`] for canonical-input revision zero.
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.canonical_input_version == 0 {
            return Err(ContractError::InvalidRange);
        }
        Ok(())
    }
}

impl TryFrom<SchemaIdentityWire> for SchemaIdentity {
    type Error = ContractError;

    fn try_from(value: SchemaIdentityWire) -> Result<Self, Self::Error> {
        Self::new(value.algorithm, value.canonical_input_version, value.digest)
    }
}

/// Serialized local-authority evidence for one negotiated schema transition.
///
/// This is output only: it has no public constructor and does not implement `Deserialize`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReviewedSchemaTransition {
    review_id: BoundedText,
    review_revision: u32,
    validated_semantics_proof_id: BoundedText,
    source: SchemaIdentity,
    target: SchemaIdentity,
    applicable_protocol: CompatibilityRange,
}

impl ReviewedSchemaTransition {
    fn from_registry_entry(
        entry: &ReviewedSchemaTransitionRegistryEntry,
        source: SchemaIdentity,
        target: SchemaIdentity,
    ) -> Result<Self, ContractError> {
        Ok(Self {
            review_id: BoundedText::new(entry.review_id)?,
            review_revision: entry.review_revision,
            validated_semantics_proof_id: BoundedText::new(entry.validated_semantics_proof_id)?,
            source,
            target,
            applicable_protocol: CompatibilityRange {
                major: entry.protocol_major,
                minimum_minor: entry.minimum_minor,
                maximum_minor: entry.maximum_minor,
            },
        })
    }

    /// Returns the stable local review identifier that authorized this transition.
    #[must_use]
    pub const fn review_id(&self) -> &BoundedText {
        &self.review_id
    }

    /// Returns the nonzero revision of the local transition review.
    #[must_use]
    pub const fn review_revision(&self) -> u32 {
        self.review_revision
    }

    /// Returns the executable proof identifier that validated the transition semantics.
    #[must_use]
    pub const fn validated_semantics_proof_id(&self) -> &BoundedText {
        &self.validated_semantics_proof_id
    }

    /// Returns the producer-side schema identity.
    #[must_use]
    pub const fn source(&self) -> &SchemaIdentity {
        &self.source
    }

    /// Returns the consumer-side schema identity.
    #[must_use]
    pub const fn target(&self) -> &SchemaIdentity {
        &self.target
    }

    /// Returns the protocol range covered by the reviewed semantics proof.
    #[must_use]
    pub const fn applicable_protocol(&self) -> CompatibilityRange {
        self.applicable_protocol
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReviewedSchemaIdentity {
    algorithm: SchemaHashAlgorithm,
    canonical_input_version: u16,
    digest: &'static str,
}

impl ReviewedSchemaIdentity {
    fn matches(self, identity: &SchemaIdentity) -> bool {
        self.algorithm == identity.algorithm
            && self.canonical_input_version == identity.canonical_input_version
            && self.digest == identity.digest.as_str()
    }

    #[cfg(test)]
    fn is_valid(self) -> bool {
        self.canonical_input_version != 0
            && self.digest.len() == 64
            && self
                .digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReviewedSchemaTransitionRegistryEntry {
    review_id: &'static str,
    review_revision: u32,
    validated_semantics_proof_id: &'static str,
    source: ReviewedSchemaIdentity,
    target: ReviewedSchemaIdentity,
    protocol_major: u16,
    minimum_minor: u16,
    maximum_minor: u16,
}

impl ReviewedSchemaTransitionRegistryEntry {
    fn matches(self, source: &SchemaIdentity, target: &SchemaIdentity) -> bool {
        self.source.matches(source) && self.target.matches(target)
    }

    const fn applies_to(self, version: ProtocolVersion) -> bool {
        self.protocol_major == version.major
            && self.minimum_minor <= version.minor
            && version.minor <= self.maximum_minor
    }

    #[cfg(test)]
    fn has_same_pair(self, other: Self) -> bool {
        self.source == other.source && self.target == other.target
    }
}

const SHIPPED_REVIEWED_SCHEMA_TRANSITIONS: &[ReviewedSchemaTransitionRegistryEntry] = &[];

/// Trusted local compatibility authority, separate from every peer and public caller.
///
/// The shipped registry is immutable and empty, so public callers can construct only strict
/// exact-schema authority. Non-empty registries can be introduced only inside this crate by a
/// reviewed source change that also supplies executable semantics proof.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SchemaCompatibilityAuthority {
    reviewed_transitions: &'static [ReviewedSchemaTransitionRegistryEntry],
}

impl SchemaCompatibilityAuthority {
    /// Returns the shipped fail-closed authority with no reviewed schema transitions.
    #[must_use]
    pub const fn strict() -> Self {
        Self {
            reviewed_transitions: SHIPPED_REVIEWED_SCHEMA_TRANSITIONS,
        }
    }

    fn transition_for(
        &self,
        source: &SchemaIdentity,
        target: &SchemaIdentity,
        version: ProtocolVersion,
    ) -> Option<&ReviewedSchemaTransitionRegistryEntry> {
        self.reviewed_transitions
            .iter()
            .find(|transition| transition.matches(source, target) && transition.applies_to(version))
    }

    #[cfg(test)]
    fn from_reviewed_registry(
        reviewed_transitions: &'static [ReviewedSchemaTransitionRegistryEntry],
    ) -> Result<Self, RegistryValidationError> {
        validate_reviewed_registry(reviewed_transitions)?;
        Ok(Self {
            reviewed_transitions,
        })
    }
}

impl Default for SchemaCompatibilityAuthority {
    fn default() -> Self {
        Self::strict()
    }
}

impl AsRef<SchemaCompatibilityAuthority> for SchemaCompatibilityAuthority {
    fn as_ref(&self) -> &SchemaCompatibilityAuthority {
        self
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RegistryValidationError {
    RegistryTooLarge,
    EmptyOrInvalidReviewId,
    InvalidReviewRevision,
    EmptyOrInvalidSemanticsProofId,
    InvalidSchemaIdentity,
    IdentityTransition,
    InvalidProtocolRange,
    DuplicateReviewKey,
    StaleReviewRevision,
    DuplicateOrAmbiguousPair,
}

#[cfg(test)]
fn validate_reviewed_registry(
    registry: &[ReviewedSchemaTransitionRegistryEntry],
) -> Result<(), RegistryValidationError> {
    if registry.len() > MAX_SCHEMA_IDENTITIES {
        return Err(RegistryValidationError::RegistryTooLarge);
    }
    for (index, entry) in registry.iter().copied().enumerate() {
        if !is_valid_registry_identifier(entry.review_id) {
            return Err(RegistryValidationError::EmptyOrInvalidReviewId);
        }
        if entry.review_revision == 0 {
            return Err(RegistryValidationError::InvalidReviewRevision);
        }
        if !is_valid_registry_identifier(entry.validated_semantics_proof_id) {
            return Err(RegistryValidationError::EmptyOrInvalidSemanticsProofId);
        }
        if !entry.source.is_valid() || !entry.target.is_valid() {
            return Err(RegistryValidationError::InvalidSchemaIdentity);
        }
        if entry.source == entry.target {
            return Err(RegistryValidationError::IdentityTransition);
        }
        if entry.protocol_major == 0 || entry.minimum_minor > entry.maximum_minor {
            return Err(RegistryValidationError::InvalidProtocolRange);
        }
        for previous in registry[..index].iter().copied() {
            if previous.review_id == entry.review_id {
                if previous.review_revision == entry.review_revision {
                    return Err(RegistryValidationError::DuplicateReviewKey);
                }
                return Err(RegistryValidationError::StaleReviewRevision);
            }
            if previous.has_same_pair(entry) {
                return Err(RegistryValidationError::DuplicateOrAmbiguousPair);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
fn is_valid_registry_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ID_BYTES
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
        })
}

/// Protocol and schema compatibility offered by one peer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, try_from = "ProtocolOfferWire")]
pub struct ProtocolOffer {
    versions: CompatibilityRange,
    accepted_schemas: Vec<SchemaIdentity>,
}

/// Legacy v1 protocol-offer wire shape retained only for explicit migration.
///
/// Its peer-controlled boolean is deliberately not exposed as an authority and is discarded
/// during migration. A current offer must still receive a local
/// [`SchemaCompatibilityAuthority`] when negotiating; the currently shipped authority still
/// requires an exact schema match.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, try_from = "ProtocolOfferV1Wire")]
pub struct ProtocolOfferV1 {
    versions: CompatibilityRange,
    accepted_schemas: Vec<SchemaIdentity>,
    allow_compatible_schema_drift: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProtocolOfferWire {
    versions: CompatibilityRange,
    accepted_schemas: Vec<SchemaIdentity>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProtocolOfferV1Wire {
    versions: CompatibilityRange,
    accepted_schemas: Vec<SchemaIdentity>,
    allow_compatible_schema_drift: bool,
}

impl ProtocolOffer {
    /// Creates a bounded peer compatibility offer.
    ///
    /// # Errors
    /// Returns [`ContractError`] when the schema set is empty or oversized.
    pub fn new(
        versions: CompatibilityRange,
        accepted_schemas: Vec<SchemaIdentity>,
    ) -> Result<Self, ContractError> {
        let offer = Self {
            versions,
            accepted_schemas,
        };
        offer.validate()?;
        Ok(offer)
    }

    /// Revalidates every offer invariant at the public decision boundary.
    ///
    /// # Errors
    ///
    /// Returns [`ContractError`] for invalid version ranges, schema identities,
    /// schema-set bounds, or duplicate schema identities.
    pub fn validate(&self) -> Result<(), ContractError> {
        self.versions.validate()?;
        if self.accepted_schemas.is_empty() {
            return Err(ContractError::Empty {
                field: "accepted_schemas",
            });
        }
        if self.accepted_schemas.len() > MAX_SCHEMA_IDENTITIES {
            return Err(ContractError::LimitExceeded {
                kind: LimitKind::SchemaSet,
                limit: MAX_SCHEMA_IDENTITIES,
                actual: self.accepted_schemas.len(),
            });
        }
        for (index, schema) in self.accepted_schemas.iter().enumerate() {
            schema.validate()?;
            if self.accepted_schemas[..index].contains(schema) {
                return Err(ContractError::DuplicateSchema);
            }
        }
        Ok(())
    }

    /// Returns the validated protocol-version range offered by this peer.
    #[must_use]
    pub const fn versions(&self) -> CompatibilityRange {
        self.versions
    }

    /// Returns the validated schema identities offered by this peer.
    #[must_use]
    pub fn accepted_schemas(&self) -> &[SchemaIdentity] {
        &self.accepted_schemas
    }

    /// Negotiates the greatest mutually supported minor and schema relationship.
    ///
    /// `self` is the source-schema producer and `peer` is the target-schema consumer. Exact
    /// identity always succeeds. Non-exact identity requires a local reviewed transition whose
    /// directed source and target pair match the two selected peer schemas exactly.
    ///
    /// # Errors
    /// Returns [`ContractError`] for major, minor, or schema incompatibility.
    pub fn negotiate<A: AsRef<SchemaCompatibilityAuthority>>(
        &self,
        peer: &Self,
        authority: A,
    ) -> Result<NegotiatedProtocol, ContractError> {
        self.validate()?;
        peer.validate()?;
        let authority = authority.as_ref();
        if self.versions.major != peer.versions.major {
            return Err(ContractError::IncompatibleMajor);
        }
        let minimum = self.versions.minimum_minor.max(peer.versions.minimum_minor);
        let maximum = self.versions.maximum_minor.min(peer.versions.maximum_minor);
        if minimum > maximum {
            return Err(ContractError::NoCompatibleMinor);
        }
        let negotiated_version = ProtocolVersion {
            major: self.versions.major,
            minor: maximum,
        };
        let exact_schema = self.accepted_schemas.iter().find(|schema| {
            peer.accepted_schemas
                .iter()
                .any(|candidate| candidate == *schema)
        });
        let schema = if let Some(identity) = exact_schema {
            SchemaDisposition::Exact(identity.clone())
        } else {
            let transition = self.accepted_schemas.iter().find_map(|source| {
                peer.accepted_schemas.iter().find_map(|target| {
                    authority
                        .transition_for(source, target, negotiated_version)
                        .map(|entry| (entry, source, target))
                })
            });
            let (entry, source, target) = transition.ok_or(ContractError::SchemaIncompatible)?;
            SchemaDisposition::ReviewedTransition(ReviewedSchemaTransition::from_registry_entry(
                entry,
                source.clone(),
                target.clone(),
            )?)
        };
        Ok(NegotiatedProtocol {
            version: negotiated_version,
            schema,
        })
    }
}

impl TryFrom<ProtocolOfferWire> for ProtocolOffer {
    type Error = ContractError;

    fn try_from(value: ProtocolOfferWire) -> Result<Self, Self::Error> {
        Self::new(value.versions, value.accepted_schemas)
    }
}

impl ProtocolOfferV1 {
    /// Migrates a legacy v1 peer offer to the current v2 offer shape.
    ///
    /// The removed v1 permissiveness boolean is intentionally ignored: peer input never grants
    /// schema-drift authority. The shipped local registry remains empty and fail-closed.
    ///
    /// # Errors
    /// Returns [`ContractError`] if the retained version or schema set is invalid for v2.
    pub fn into_v2(self) -> Result<ProtocolOffer, ContractError> {
        ProtocolOffer::new(self.versions, self.accepted_schemas)
    }

    /// Returns the legacy peer value for observability only; it has no authority effect.
    #[must_use]
    pub const fn legacy_allow_compatible_schema_drift(&self) -> bool {
        self.allow_compatible_schema_drift
    }
}

impl TryFrom<ProtocolOfferV1Wire> for ProtocolOfferV1 {
    type Error = ContractError;

    fn try_from(value: ProtocolOfferV1Wire) -> Result<Self, Self::Error> {
        ProtocolOffer::new(value.versions, value.accepted_schemas).map(|current| Self {
            versions: current.versions,
            accepted_schemas: current.accepted_schemas,
            allow_compatible_schema_drift: value.allow_compatible_schema_drift,
        })
    }
}

/// Result of schema compatibility negotiation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaDisposition {
    Exact(SchemaIdentity),
    ReviewedTransition(ReviewedSchemaTransition),
}

/// Negotiated protocol version and schema relationship.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NegotiatedProtocol {
    pub version: ProtocolVersion,
    pub schema: SchemaDisposition,
}

#[cfg(test)]
mod authority_tests {
    use super::*;

    const SOURCE: ReviewedSchemaIdentity = ReviewedSchemaIdentity {
        algorithm: SchemaHashAlgorithm::Sha256,
        canonical_input_version: 1,
        digest: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    };
    const TARGET: ReviewedSchemaIdentity = ReviewedSchemaIdentity {
        algorithm: SchemaHashAlgorithm::Sha256,
        canonical_input_version: 2,
        digest: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    };
    const UNRELATED: ReviewedSchemaIdentity = ReviewedSchemaIdentity {
        algorithm: SchemaHashAlgorithm::Sha256,
        canonical_input_version: 3,
        digest: "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
    };

    const fn entry(
        review_id: &'static str,
        review_revision: u32,
        validated_semantics_proof_id: &'static str,
        source: ReviewedSchemaIdentity,
        target: ReviewedSchemaIdentity,
        minimum_minor: u16,
        maximum_minor: u16,
    ) -> ReviewedSchemaTransitionRegistryEntry {
        ReviewedSchemaTransitionRegistryEntry {
            review_id,
            review_revision,
            validated_semantics_proof_id,
            source,
            target,
            protocol_major: 1,
            minimum_minor,
            maximum_minor,
        }
    }

    const AUTHORIZED_ENTRY: ReviewedSchemaTransitionRegistryEntry = entry(
        "ff.review.diagnostic-schema-a-to-b",
        2,
        "ff.proof.diagnostic-schema-a-to-b-v2",
        SOURCE,
        TARGET,
        1,
        2,
    );
    static AUTHORIZED_REGISTRY: [ReviewedSchemaTransitionRegistryEntry; 1] = [AUTHORIZED_ENTRY];

    fn schema(identity: ReviewedSchemaIdentity) -> SchemaIdentity {
        SchemaIdentity::new(
            identity.algorithm,
            identity.canonical_input_version,
            SchemaHash::new(identity.digest).expect("test digest must be canonical"),
        )
        .expect("test schema identity must be valid")
    }

    fn offer(
        identity: ReviewedSchemaIdentity,
        minimum_minor: u16,
        maximum_minor: u16,
    ) -> ProtocolOffer {
        ProtocolOffer::new(
            CompatibilityRange::new(1, minimum_minor, maximum_minor)
                .expect("test range must be valid"),
            vec![schema(identity)],
        )
        .expect("test offer must be valid")
    }

    #[test]
    fn internal_registry_authorizes_only_its_directed_pair() {
        let authority = SchemaCompatibilityAuthority::from_reviewed_registry(&AUTHORIZED_REGISTRY)
            .expect("reviewed registry must validate");
        let source = offer(SOURCE, 1, 2);
        let target = offer(TARGET, 2, 3);
        let negotiated = source
            .negotiate(&target, authority)
            .expect("reviewed directed transition must negotiate");
        assert_eq!(negotiated.version, ProtocolVersion { major: 1, minor: 2 });
        assert!(matches!(
            negotiated.schema,
            SchemaDisposition::ReviewedTransition(ref transition)
                if transition.review_id().as_str() == "ff.review.diagnostic-schema-a-to-b"
                    && transition.review_revision() == 2
                    && transition.validated_semantics_proof_id().as_str()
                        == "ff.proof.diagnostic-schema-a-to-b-v2"
                    && transition.source() == &schema(SOURCE)
                    && transition.target() == &schema(TARGET)
                    && transition.applicable_protocol()
                        == CompatibilityRange { major: 1, minimum_minor: 1, maximum_minor: 2 }
        ));

        assert_eq!(
            target.negotiate(&source, authority),
            Err(ContractError::SchemaIncompatible)
        );
        assert_eq!(
            source.negotiate(&offer(UNRELATED, 2, 3), authority),
            Err(ContractError::SchemaIncompatible)
        );
    }

    #[test]
    fn internal_registry_rejects_out_of_authority_negotiated_minor() {
        let authority = SchemaCompatibilityAuthority::from_reviewed_registry(&AUTHORIZED_REGISTRY)
            .expect("reviewed registry must validate");
        assert_eq!(
            offer(SOURCE, 1, 3).negotiate(&offer(TARGET, 1, 3), authority),
            Err(ContractError::SchemaIncompatible)
        );
    }

    #[test]
    fn registry_validation_rejects_duplicate_keys_and_ambiguous_pairs() {
        let duplicate_key = [
            AUTHORIZED_ENTRY,
            entry(
                AUTHORIZED_ENTRY.review_id,
                AUTHORIZED_ENTRY.review_revision,
                "ff.proof.different",
                SOURCE,
                UNRELATED,
                1,
                2,
            ),
        ];
        assert_eq!(
            validate_reviewed_registry(&duplicate_key),
            Err(RegistryValidationError::DuplicateReviewKey)
        );

        let ambiguous_pair = [
            AUTHORIZED_ENTRY,
            entry(
                "ff.review.competing-a-to-b",
                1,
                "ff.proof.competing-a-to-b",
                SOURCE,
                TARGET,
                2,
                4,
            ),
        ];
        assert_eq!(
            validate_reviewed_registry(&ambiguous_pair),
            Err(RegistryValidationError::DuplicateOrAmbiguousPair)
        );
    }

    #[test]
    fn registry_validation_rejects_invalid_and_stale_metadata() {
        assert_eq!(
            validate_reviewed_registry(&[entry(
                "ff.review.zero-revision",
                0,
                "ff.proof.zero-revision",
                SOURCE,
                TARGET,
                1,
                2,
            )]),
            Err(RegistryValidationError::InvalidReviewRevision)
        );
        assert_eq!(
            validate_reviewed_registry(&[
                AUTHORIZED_ENTRY,
                entry(
                    AUTHORIZED_ENTRY.review_id,
                    AUTHORIZED_ENTRY.review_revision + 1,
                    "ff.proof.newer",
                    SOURCE,
                    UNRELATED,
                    1,
                    2,
                ),
            ]),
            Err(RegistryValidationError::StaleReviewRevision)
        );
        assert_eq!(
            validate_reviewed_registry(&[
                entry("ff.review.no-proof", 1, "", SOURCE, TARGET, 1, 2,)
            ]),
            Err(RegistryValidationError::EmptyOrInvalidSemanticsProofId)
        );
        assert_eq!(
            validate_reviewed_registry(&[entry(
                "ff.review.bad-range",
                1,
                "ff.proof.bad-range",
                SOURCE,
                TARGET,
                4,
                3,
            )]),
            Err(RegistryValidationError::InvalidProtocolRange)
        );
    }

    #[test]
    fn registry_validation_rejects_oversized_registry() {
        let oversized = [AUTHORIZED_ENTRY; MAX_SCHEMA_IDENTITIES + 1];
        assert_eq!(
            validate_reviewed_registry(&oversized),
            Err(RegistryValidationError::RegistryTooLarge)
        );
    }

    #[test]
    fn negotiation_revalidates_offers_constructed_inside_the_crate() {
        let valid = offer(SOURCE, 0, 1);
        let invalid_range = ProtocolOffer {
            versions: CompatibilityRange {
                major: 0,
                minimum_minor: 0,
                maximum_minor: 1,
            },
            accepted_schemas: vec![schema(SOURCE)],
        };
        assert_eq!(
            invalid_range.negotiate(&valid, SchemaCompatibilityAuthority::strict()),
            Err(ContractError::InvalidRange)
        );

        let invalid_schema = ProtocolOffer {
            versions: CompatibilityRange {
                major: 1,
                minimum_minor: 0,
                maximum_minor: 1,
            },
            accepted_schemas: vec![SchemaIdentity {
                algorithm: SchemaHashAlgorithm::Sha256,
                canonical_input_version: 0,
                digest: SchemaHash::new(SOURCE.digest).expect("test digest must be canonical"),
            }],
        };
        assert_eq!(
            valid.negotiate(&invalid_schema, SchemaCompatibilityAuthority::strict()),
            Err(ContractError::InvalidRange)
        );
    }
}

/// Stable stream identity excluding its sequence number.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceKey {
    pub producer_instance: ProducerInstanceId,
    pub boot_session: BootSessionId,
    pub channel: ChannelId,
}

/// Full event identity used for ordering and durable acknowledgements.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, try_from = "SequenceIdentityWire")]
pub struct SequenceIdentity {
    pub key: SequenceKey,
    pub sequence: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SequenceIdentityWire {
    key: SequenceKey,
    sequence: u64,
}

impl SequenceIdentity {
    /// Creates a legal nonzero sequence identity.
    ///
    /// # Errors
    /// Returns [`ContractError`] for sequence zero.
    pub fn new(key: SequenceKey, sequence: u64) -> Result<Self, ContractError> {
        if !(FIRST_SEQUENCE..=LAST_SEQUENCE).contains(&sequence) {
            return Err(ContractError::Sequence {
                fault: SequenceFault::InvalidStart,
            });
        }
        Ok(Self { key, sequence })
    }

    /// Revalidates a sequence identity that may have been constructed in memory.
    ///
    /// # Errors
    /// Returns [`ContractError`] when the sequence is zero.
    pub fn validate(&self) -> Result<(), ContractError> {
        if !(FIRST_SEQUENCE..=LAST_SEQUENCE).contains(&self.sequence) {
            return Err(ContractError::Sequence {
                fault: SequenceFault::InvalidStart,
            });
        }
        Ok(())
    }

    /// Advances without wrapping.
    ///
    /// # Errors
    /// Returns [`ContractError`] when the sequence space is exhausted.
    pub fn checked_next(&self) -> Result<Self, ContractError> {
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or(ContractError::Sequence {
                fault: SequenceFault::Exhausted,
            })?;
        Self::new(self.key.clone(), sequence)
    }
}

impl TryFrom<SequenceIdentityWire> for SequenceIdentity {
    type Error = ContractError;

    fn try_from(value: SequenceIdentityWire) -> Result<Self, Self::Error> {
        Self::new(value.key, value.sequence)
    }
}

/// Successful admission classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SequenceDisposition {
    First,
    Contiguous,
    Replay,
}

/// Pure sequence validator. It stores only admitted and durable positions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SequenceTracker {
    key: SequenceKey,
    last_admitted: Option<u64>,
    last_durable: Option<u64>,
}

impl SequenceTracker {
    #[must_use]
    pub const fn new(key: SequenceKey) -> Self {
        Self {
            key,
            last_admitted: None,
            last_durable: None,
        }
    }

    /// Admits the first or next contiguous identity.
    ///
    /// # Errors
    /// Returns [`ContractError`] for identity changes, bad starts, duplicates, gaps, or reordering.
    pub fn admit(
        &mut self,
        identity: &SequenceIdentity,
    ) -> Result<SequenceDisposition, ContractError> {
        self.ensure_key(identity)?;
        match self.last_admitted {
            None if identity.sequence == FIRST_SEQUENCE => {
                self.last_admitted = Some(identity.sequence);
                Ok(SequenceDisposition::First)
            }
            None => Err(ContractError::Sequence {
                fault: SequenceFault::InvalidStart,
            }),
            Some(last) if identity.sequence == last => Err(ContractError::Sequence {
                fault: SequenceFault::Duplicate,
            }),
            Some(last) if identity.sequence < last => Err(ContractError::Sequence {
                fault: SequenceFault::Reordered,
            }),
            Some(last) => {
                let expected = last.checked_add(1).ok_or(ContractError::Sequence {
                    fault: SequenceFault::Exhausted,
                })?;
                if identity.sequence != expected {
                    return Err(ContractError::Sequence {
                        fault: SequenceFault::Gap,
                    });
                }
                self.last_admitted = Some(identity.sequence);
                Ok(SequenceDisposition::Contiguous)
            }
        }
    }

    /// Validates an explicit reconnect replay without advancing admission.
    ///
    /// # Errors
    /// Returns [`ContractError`] if no stream exists or the identity is outside the replay window.
    pub fn admit_replay(
        &self,
        identity: &SequenceIdentity,
        replay_window: u64,
    ) -> Result<SequenceDisposition, ContractError> {
        identity.validate()?;
        self.ensure_key(identity)?;
        let last = self.last_admitted.ok_or(ContractError::Sequence {
            fault: SequenceFault::ReplayOutsideWindow,
        })?;
        if identity.sequence > last || last.saturating_sub(identity.sequence) >= replay_window {
            return Err(ContractError::Sequence {
                fault: SequenceFault::ReplayOutsideWindow,
            });
        }
        Ok(SequenceDisposition::Replay)
    }

    /// Advances durability without passing admitted data or moving backward.
    ///
    /// # Errors
    /// Returns [`ContractError`] for ahead-of-admission or reordered acknowledgement.
    pub fn acknowledge_durable(&mut self, sequence: u64) -> Result<(), ContractError> {
        if !(FIRST_SEQUENCE..=LAST_SEQUENCE).contains(&sequence) {
            return Err(ContractError::Sequence {
                fault: SequenceFault::InvalidStart,
            });
        }
        let admitted = self.last_admitted.ok_or(ContractError::Sequence {
            fault: SequenceFault::DurableAheadOfAdmitted,
        })?;
        if sequence > admitted {
            return Err(ContractError::Sequence {
                fault: SequenceFault::DurableAheadOfAdmitted,
            });
        }
        if self.last_durable.is_some_and(|durable| sequence < durable) {
            return Err(ContractError::Sequence {
                fault: SequenceFault::DurableReordered,
            });
        }
        self.last_durable = Some(sequence);
        Ok(())
    }

    #[must_use]
    pub const fn last_admitted(&self) -> Option<u64> {
        self.last_admitted
    }

    #[must_use]
    pub const fn last_durable(&self) -> Option<u64> {
        self.last_durable
    }

    fn ensure_key(&self, identity: &SequenceIdentity) -> Result<(), ContractError> {
        if identity.key != self.key {
            return Err(ContractError::Sequence {
                fault: SequenceFault::IdentityChanged,
            });
        }
        Ok(())
    }
}
