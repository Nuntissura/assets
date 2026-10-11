//! SVG/AI vector import/export with fallible loss-aware proposals; PDF decoding/editing exposed
//! through Press provider ports, not unconditional all-PDF engine.
#![forbid(unsafe_code)]

mod ai_adapter;
mod error;
mod ids;
mod model;
mod pdf_port;
mod svg_adapter;

pub use ai_adapter::{AiKind, identify_ai, import_ai};
pub use error::{PreservedBlob, Result, VectorError};
pub use ids::{IdMint, SequentialIdMint};
pub use model::*;
pub use pdf_port::{NoPdfProvider, PdfVectorProvider};
pub use svg_adapter::{export_svg, import_svg};

use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{Observation, Observe, Outcome, Receipt, SinkPort};

/// Maps an import/export result to exactly one Observe terminal outcome (no ids, no element text).
/// A proposal that carries losses is still `Success`; the loss list is the detail. Blocked native
/// editability and a missing PDF provider are `Failure(Unavailable)`, unsupported formats
/// `Failure(Unsupported)`, budget/invalid input `Failure(Validation)`, cancellation `Canceled`.
/// `correlation_id` and `revision` must equal the values `observe` was created with.
pub fn report<T>(
    result: &Result<T>,
    correlation_id: u64,
    revision: u64,
    cancel: &CancellationToken,
    observe: &mut Observe,
    sink: &mut (impl SinkPort + ?Sized),
) -> std::result::Result<Receipt, hsk_studio_observe::Error> {
    let outcome = match result {
        Ok(_) => Outcome::Success,
        Err(error) => match error.failure_code() {
            None => Outcome::Canceled,
            Some(code) => Outcome::Failure(code),
        },
    };
    observe.emit(
        Observation {
            correlation_id,
            revision,
            outcome,
            progress: None,
            private_project_text: None,
        },
        cancel,
        sink,
    )
}
