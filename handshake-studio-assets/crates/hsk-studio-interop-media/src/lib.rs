//! Typed timeline/motion/audio interchange (PRPROJ/AEP/SESX included explicit missing-decoder
//! scope), loss-aware proposals through neutral Folio contracts. No Lightroom parser or private
//! catalog; lrcat has its own adapter.
#![forbid(unsafe_code)]

/// SubRip / WebVTT caption interchange (lane SMB-PREMIERE-COLOR-TITLES).
pub mod captions;
mod edl;
mod error;
mod folio_map;
mod model;
mod native;
mod time;

pub use edl::*;
pub use error::{ErrorKind, InteropError, Result};
pub use folio_map::*;
pub use model::*;
pub use native::*;
pub use time::*;

use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{Observation, Observe, Outcome, Receipt, SinkPort};

/// Maps an import/export result to exactly one Observe terminal outcome (no ids, no reel or
/// project text). `correlation_id` and `revision` must equal the values `observe` was created with.
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

/// Same descriptor for models, human manual and Argus/diagnostic adapters.
pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-INTEROP_MEDIA","version":1,"operation":"import_cmx3600(bytes, ImportRequest, Limits) -> TimelineProposal (neutral, with typed losses); build_sequence(TimelineProposal, SequenceOptions, IdSource) -> Folio StudioSequence; propose_document_revision(Snapshot, expected_revision, BuiltSequence, FolioContext) -> Folio-validated prospective Snapshot at revision+1; export_cmx3600(TimelineProposal) -> deterministic text; sniff_native / import_native for PRPROJ, AEP, SESX (detection only, editable import blocked)","operator":{"intent":"Bring edit decisions from an EDL into a Studio sequence without silent loss","result":"A proposal listing every clip with its disposition and every dropped or approximated property as a loss note; nothing is committed","recovery":"invalid_line / invalid_range / length_mismatch / overlapping_events: fix the named line in the source; dropped_timecode: the label does not exist in drop-frame; drop_mismatch: a semicolon timecode needs FCM: DROP FRAME; unsupported_frame_rate: use a STU-VID-013 rate; native_decoder_missing: keep the original file as an opaque blob, editable import is blocked; stale_revision: reload the document and re-propose; canceled: nothing was produced"},"model":{"time":"timecode -> frame number -> ticks at 254016000000 per second (frames * ticks per frame); record timecode may be shifted by a record origin; source timecode stays absolute; drop-frame labels follow the published skip rule and never change ticks; rational_to_ticks is exact or rounds half away from zero and reports Approximate; legacy ticks per frame 10594594594 and 8475675675 normalize with an Exactness receipt","supported":"CMX3600 cuts on V, A, A2.., AA, B, AA/V; M2 speed as an exact rational (reverse = negative); FROM and TO CLIP NAME; TITLE; FCM","dispositions":"editable, translated (speed), approximate (collapsed stereo pair, M2 disagreeing with spans), unsupported (dissolve, wipe, key, freeze frame, unknown track, other directives), missing (unknown reel, offline placeholder, native decoder)","ids":"this crate mints no ids: IdSource and the reel map are caller supplied; SequentialIds is deterministic and uuid_v7-shaped","consumer":"interop-media-consumer [--descriptor] [--edl FILE --rate RATE [--drop] [--origin FRAMES] [--export]] [--sniff FILE]","errors":"InteropError::code() snake_case with the 1-based source line when one applies; canceled maps to the Observe Canceled outcome, unsupported kinds to Failure Unsupported, the rest to Failure Validation"},"limits":{"max_bytes":8388608,"max_events":100000,"max_line_bytes":1024,"case_seconds":30},"cancel":"CancellationToken is checked at entry and per event; a canceled call returns canceled and no partial proposal","undo":"imports are proposals only; the host records an applied revision as one reversible step","argus":{"inspect":"TimelineProposal clips and losses, DocumentProposal snapshot","action":"the same import and propose functions with the same cancellation token","capture":"caller captures the source bytes, proposal and Observe frame under a fresh grant"},"diagnostics":"report() emits one Observe terminal outcome; no ids, reel names, paths or project text are serialized by this crate","native_gap":"PRPROJ, AEP and SESX have no decoder: detection strings are unverified candidates, every entity class is reported Missing, parser success is never editable parity (STU-IO-186)"}"#;
