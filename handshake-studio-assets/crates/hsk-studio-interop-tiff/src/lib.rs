//! Studio TIFF interop: the single TIFF container owner (lead decision F1, 2026-10-14; CX-CAP-001).
//!
//! Owns TIFF container parsing, decoding and encoding: IFD chain, strips and tiles, decompression,
//! predictors, sample formats, ICC (tag 34675), ExtraSamples, multi-page, BigTIFF, and raw access to the
//! Photoshop tags 37724 (ImageSourceData) and 34377 (ImageResources).
//!
//! Folio-free: the only Studio edges are `hsk-studio-accord` and `hsk-studio-observe`, so the develop engine
//! can consume this crate without gaining a Folio path. Mapping a decoded TIFF into a Studio document belongs
//! to the convert-to-native flow, and the layer records inside tag 37724 are parsed by interop-psd.
#![forbid(unsafe_code)]
#![deny(clippy::arithmetic_side_effects)]

mod decode;
mod encode;
mod error;
mod limits;
mod model;
mod scan;

pub use decode::decode;
pub use encode::{encode, ColorModel, Compression, EncodeRequest, Predictor};
pub use error::{Result, TiffError};
pub use limits::{Limits, MAX_ENCODE_BYTES};
pub use model::{
    Alpha, ByteOrder, PageInfo, Photometric, Rational, RawBlock, Resolution, SampleFormat, Samples, TiffFile,
    TiffPage,
};

use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{Observation, Observe, Outcome, Receipt, SinkPort};

/// The complete public surface (everything else is private).
pub const PUBLIC_ENTRYPOINTS: &[&str] = &["decode", "encode", "report"];

/// Maps a decode/encode result to exactly one Observe terminal outcome (no ids, paths or pixel data).
/// Unsupported features are `Failure(Unsupported)`, cancellation `Canceled`, everything else
/// `Failure(Validation)`. `correlation_id` and `revision` must equal the values `observe` was created with.
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
        Observation { correlation_id, revision, outcome, progress: None, private_project_text: None },
        cancel,
        sink,
    )
}

/// Same descriptor for models, the human manual and Argus/diagnostic adapters.
pub const DESCRIPTOR: &str = "";
