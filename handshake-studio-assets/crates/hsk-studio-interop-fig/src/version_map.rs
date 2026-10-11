//! Version policy: which (kind, version) pairs a caller admits for decoding. The container reader
//! never judges versions. The default policy admits NOTHING: no Figma-authored file exists on this
//! machine, so no real version value has been observed; a version is added only from a file that was
//! actually read (record it in `OBSERVED_VERSIONS` with its provenance). Callers and synthetic tests
//! opt in explicitly through [`VersionPolicy::with_supported`].
use crate::container::{FigContainer, FigKind};
use crate::{FigError, Result};

/// Versions seen in real Figma-authored files, with provenance. Empty until the operator supplies
/// files (`tests/fixtures/*.fig`).
pub const OBSERVED_VERSIONS: &[(FigKind, u32, &str)] = &[];

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
        !matches!(kind, FigKind::Unknown(_)) && self.supported.contains(&(kind, version))
    }
}

/// `Err(UnsupportedVersion)` unless the container's (kind, version) is admitted. An unclassified
/// kind is never admitted. The caller keeps the container opaque-preserved: nothing is decoded and
/// no partial import exists.
pub fn check_version(container: &FigContainer, policy: &VersionPolicy) -> Result<()> {
    if policy.admits(container.kind, container.version) {
        Ok(())
    } else {
        Err(FigError::UnsupportedVersion {
            kind: container.kind,
            found: container.version,
        })
    }
}
