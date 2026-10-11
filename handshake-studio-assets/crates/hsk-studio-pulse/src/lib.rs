//! Canonical integer-tick sequence time at 254016000000 ticks/second per STU-VID-012 and normalized
//! broadcast ticks/frame per STU-VID-013. Preserve declared field types with checked
//! arithmetic/range; rational time is for interchange adapters, never a competing stored authority.
//! Own edit/trim/ripple/multicam references and scheduling; media decoder behind typed port.
#![forbid(unsafe_code)]

mod error;
mod range;
mod rational;
mod time;

pub use error::{PulseError, Result};
pub use range::*;
pub use rational::*;
pub use time::*;

use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{Observation, Observe, Outcome, Receipt, SinkPort};

/// Maps a pulse result to exactly one Observe terminal outcome (no project text, no ids).
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

/// Same descriptor for models, human manual and Argus/diagnostic adapters.
pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-PULSE","version":1,"operation":"integer-tick time algebra over accord::Ticks: frame rate table and legacy normalization, frame/tick and sample/tick conversion, drop-frame and non-drop timecode label functions, rational interchange with explicit rounding, signed TickDelta, TickRange, checked trim and ripple over an ordered lane of ranges","operator":{"intent":"Move, trim and ripple sequence time without float drift or silent rounding","result":"New range lane or tick value, or a stable error code; inputs are never modified","recovery":"overlap: the move collides with a neighbour, shorten it or ripple; underflow: the move passes time zero; overflow: value exceeds u64 ticks; not_frame_aligned: use whole frames on a frame grid; inexact: choose Floor/Ceil/NearestHalfUp or a table rate; invalid_timecode: drop-frame skips that label; canceled: nothing was produced"},"model":{"time":"u64 ticks at 254016000000 per second (accord::Ticks); ticks per frame from the STU-VID-013 table; legacy 10594594594 and 8475675675 normalize to 10594584000 and 8475667200 with a FrameRateNormalization receipt; drop-frame (2 frame numbers per minute at 29.97, 4 at 59.94, none every tenth minute) only labels frame numbers and never changes ticks or frame counts","inputs":"Ticks, FrameRate, TickRange [start,end), TickDelta (signed, checked), Grid::Free or Grid::Frames(rate), ordered non-overlapping lanes of TickRange, Speed (reduced rational, sign = reverse), Rounding for rational seconds","consumer":"pulse-consumer [--descriptor] [--rate TICKS_PER_FRAME] [--drop] [--frame N] [--a-frames N] [--b-frames N] [--trim-to-frames N] [--ripple] [--cancel]","errors":"PulseError::code() snake_case; canceled maps to the Observe Canceled outcome, overflow/unsupported/inexact to Failure Unsupported, everything else to Failure Validation"},"limits":{"ranges_per_lane":262144,"case_seconds":30},"cancel":"CancellationToken is checked at entry and every 256 ranges; a canceled call returns canceled and produces nothing","undo":"pulse is pure and holds no history; the host records each applied result as one reversible step","argus":{"inspect":"all values are plain data","action":"the same functions with the same cancellation token","capture":"caller captures inputs and results under a fresh grant"},"diagnostics":"report() emits one Observe terminal outcome; no ids, paths or project text are serialized by pulse"}"#;
