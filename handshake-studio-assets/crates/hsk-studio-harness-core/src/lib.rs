//! Core half of STUDIO-MODULE-HARNESS (SMB-CRATE-SEAM-SPLITS slice 3b): the headless run over the
//! real session (open, revision-checked provider submit, atomic save, reopen), the import routes
//! (features import, import-psd, import-raw, import-tiff, import-pdf, import-raster), the
//! controls-descriptor panel and the manual. `hsk-studio-harness` is the public face; the
//! `#[doc(hidden)]` module is the seam `hsk-studio-harness-view` and the CLI build on.
#![forbid(unsafe_code)]

pub mod action;
pub mod batch;
pub mod context;
/// Lane SMB-R53-FOLLOWUPS-1 (F-R53-1): Folio's swatch-group and colour-book reports through observe.
pub mod folio_report;
#[doc(hidden)]
pub mod harness;
pub mod import;
pub mod manual;
/// Lane SMB-R53-FOLLOWUPS-1 (MT-31251): the Prism named-colour profile -> Folio colour-book join;
/// Prism is reached only with an import feature that enables it.
#[cfg(any(feature = "import-psd", feature = "import-raw", feature = "import-tiff", feature = "import-raster"))]
pub mod named_colour_book;
pub mod panel;

pub use action::{ActionRecord, ActionSet, ActionStep, PlayMode, RecordedValue, RecordingReport};
pub use batch::{BatchJob, BatchReport};
pub use harness::{AVAILABLE_PROVIDERS, Harness, HarnessError, NOT_PROVEN, Place, PlacementWrite, References, Run, SCHEMA, Selection};
// Lane SMB-ATTRIBUTION-1: the run's attribution and accord's command-context types, re-exported so
// callers (harness-view, the CLI) need no further edge.
pub use context::{RunContext, SharedBusinessEventSink};
#[cfg(feature = "asset-local-test-host")]
pub use context::asset_local_test_context;
pub use hsk_studio_accord::{CausationId, ClientSessionId, CommandContext, CommandId, CorrelationId, CorrelationSource, TransientCorrelation};
// Lane SMB-BUSINESS-EVENTS-1: accord's business-event vocabulary, re-exported for callers of Run.
pub use hsk_studio_accord::{BusinessEventDeliveryError, BusinessEventRecord, BusinessEventSink, StudioBusinessEventId};
