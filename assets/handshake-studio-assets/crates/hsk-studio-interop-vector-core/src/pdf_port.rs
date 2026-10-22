//! PDF vector import is a provider port: decoding belongs to Press (STUDIO_MODULES), which has no
//! dependency edge here. A host or harness adapter implements the trait; this crate only fixes the
//! contract and the explicit "no provider selected" answer.
use crate::error::{Result, VectorError};
use crate::model::VectorProposal;
use hsk_studio_accord::CancellationToken;

pub trait PdfVectorProvider: Send + Sync {
    /// Imports the vector content of one zero-based page as a loss-aware proposal.
    fn import_page(&self, pdf: &[u8], page: u32, cancel: &CancellationToken)
    -> Result<VectorProposal>;
}

/// The explicit absence of a provider: every call answers `PdfUnavailable`.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoPdfProvider;

impl PdfVectorProvider for NoPdfProvider {
    fn import_page(&self, _: &[u8], _: u32, cancel: &CancellationToken) -> Result<VectorProposal> {
        cancel.check().map_err(|_| VectorError::Canceled)?;
        Err(VectorError::PdfUnavailable)
    }
}
