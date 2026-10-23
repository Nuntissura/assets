//! Version policy: which (kind, version) pairs a caller admits for decoding. The container reader
//! does not judge versions (test: unsupported_version_duplicate_key_bomb_cancel). The default policy
//! admits NOTHING; [`VersionPolicy::observed`] admits only `OBSERVED_VERSIONS`, which lists a version
//! only after it was read in a real file (record it with its provenance). Callers and synthetic
//! tests opt in explicitly through [`VersionPolicy::with_supported`].
use crate::container::{FigContainer, FigKind};
use crate::{FigError, Result};

/// Versions seen in real Figma-authored files, with provenance: the three Operator-authored files
/// in `tests/fixtures` (Figma desktop 126.10.7, "Save local copy", 2026-10-14; `provenance.json`)
/// all carry `fig-kiwi` version 106. No `fig-jam.` or `fig-deck` file has been observed.
pub const OBSERVED_VERSIONS: &[(FigKind, u32, &str)] = &[(
    FigKind::Design,
    106,
    "tests/fixtures/*.fig, Figma desktop 126.10.7, 2026-10-14",
)];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VersionPolicy {
    supported: Vec<(FigKind, u32)>,
}

impl VersionPolicy {
    /// Admits only the versions in `OBSERVED_VERSIONS` (currently none).
    pub fn observed() -> Self {
        Self {
            supported: OBSERVED_VERSIONS.iter().map(|(k, v, _)| (*k, *v)).collect(),
        }
    }
    /// Adds one admitted pair (tests: synthetic fixtures only; production: observed versions only).
    pub fn with_supported(mut self, kind: FigKind, version: u32) -> Self {
        if !self.supported.contains(&(kind, version)) {
            self.supported.push((kind, version));
        }
        self
    }
    pub fn supported(&self) -> &[(FigKind, u32)] {
        &self.supported
    }
    pub fn admits(&self, kind: FigKind, version: u32) -> bool {
        !matches!(kind, FigKind::Unknown(_) | FigKind::Diff)
            && self.supported.contains(&(kind, version))
    }
}

/// `Err(UnsupportedVersion)` unless the container's (kind, version) is admitted. An unclassified
/// kind is not admitted (test: legacy_bare_and_foreign_kinds). The caller keeps the container opaque-preserved: nothing is decoded and
/// no partial import exists.
pub fn check_version(container: &FigContainer, policy: &VersionPolicy) -> Result<()> {
    if container.kind == FigKind::Diff {
        return Err(FigError::UnsupportedKind {
            kind: container.kind,
        });
    }
    if policy.admits(container.kind, container.version) {
        Ok(())
    } else {
        Err(FigError::UnsupportedVersion {
            kind: container.kind,
            found: container.version,
        })
    }
}
