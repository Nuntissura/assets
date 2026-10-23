//! Uncompressed PCM in Matroska, decoded through the shared PCM path (`isobmff::PcmStream` +
//! `deliver_pcm_stream`).
//!
//! Codec ids and layouts (Matroska codec specs, matroska.org/technical/codec_specs.html, verified
//! 2026-10-11): `A_PCM/INT/LIT` and `A_PCM/INT/BIG` are integer PCM, little/big endian, signed except at
//! bit depth 8 where samples MUST be read as unsigned; `A_PCM/FLOAT/IEEE` is IEEE 754 float, stored
//! little-endian. `BitDepth`, `Channels` and `SamplingFrequency` of the Audio element define the layout and
//! are checked and refused when unsupported.
//!
//! Timing: a PCM stream is continuous, so the position of a frame is the running count of frames, not the
//! Block timestamp (muxers round block times to the TimestampScale: 1024 frames at 48 kHz is 21.33 ms).
//! The Block timestamps are only used as a consistency check: a block that starts more than one
//! TimestampScale unit away from where the running count says it should start is a discontinuity
//! (dropped or repeated audio) and makes the track refuse to decode (`Unsupported`); samples are not shifted
//! silently (test `matroska_pcm_and_content_encoding_hostile_inputs_refuse_cleanly`). All arithmetic is checked (V4-01).
#![deny(clippy::arithmetic_side_effects)]
use crate::error::ReelError;
use crate::isobmff::{PcmFormat, PcmStream, PcmWindow, SampleEntry};
use crate::matroska::MkvTrackInfo;
use crate::time::{exact_ticks_per_unit, media_to_ticks};

fn range() -> ReelError {
    ReelError::LimitExceeded {
        what: "pcm-frame-count",
    }
}

/// The PCM layout a Matroska audio track declares. `Ok(None)`: not a PCM CodecID. `Err`: a PCM CodecID
/// whose parameters this reader cannot honour.
pub(crate) fn pcm_format_of(info: &MkvTrackInfo) -> Result<Option<PcmFormat>, ReelError> {
    let (float, big_endian) = match info.codec_id.as_str() {
        "A_PCM/INT/LIT" => (false, false),
        "A_PCM/INT/BIG" => (false, true),
        "A_PCM/FLOAT/IEEE" => (true, false),
        _ => return Ok(None),
    };
    let rate = info.sampling_frequency;
    if !(rate.is_finite() && rate >= 1.0 && rate <= f64::from(u32::MAX) && rate.fract() == 0.0) {
        return Err(ReelError::Unsupported {
            what: "pcm-sample-rate",
        });
    }
    // Integer 8-bit PCM is unsigned by the codec spec; every other layout is signed.
    let signed = float || info.bit_depth != 8;
    PcmFormat::validated(
        info.bit_depth,
        float,
        big_endian,
        signed,
        info.channels,
        rate as u32,
    )
    .map(Some)
}

/// Compact stream over the track's blocks (`samples` in file order, `units_per_second` = timestamps per
/// second from TimestampScale). `Ok(None)` when the track is not (valid) PCM: the typed reason is
/// reported again at delivery by `pcm_format_of`.
pub(crate) fn build_pcm_stream(
    info: &MkvTrackInfo,
    samples: &[SampleEntry],
    units_per_second: u32,
) -> Result<Option<PcmStream>, ReelError> {
    let Ok(Some(format)) = pcm_format_of(info) else {
        return Ok(None);
    };
    let block = format.block_bytes() as u64;
    let mut blocks = Vec::with_capacity(samples.len());
    let mut verdict: Result<(), ReelError> = Ok(());
    let rate = i128::from(format.rate);
    let per_second = i128::from(units_per_second);
    let first_pts = samples.first().map_or(0, |s| s.pts);
    let mut total: u64 = 0;
    for sample in samples {
        let size = u64::from(sample.size);
        let frames = size.checked_div(block).unwrap_or(0);
        if verdict.is_ok() {
            if size.checked_rem(block) != Some(0) {
                verdict = Err(ReelError::Corrupt {
                    what: "pcm-block-size",
                });
            } else {
                // |(ts - ts0) * rate - frames_so_far * units_per_second| <= rate  (one timestamp unit).
                let drift = i128::from(sample.pts)
                    .checked_sub(i128::from(first_pts))
                    .and_then(|dt| dt.checked_mul(rate))
                    .zip(i128::from(total).checked_mul(per_second))
                    .and_then(|(have, want)| have.checked_sub(want));
                if drift.is_none_or(|d| d.unsigned_abs() > rate.unsigned_abs()) {
                    verdict = Err(ReelError::Unsupported {
                        what: "pcm-timestamp-discontinuity",
                    });
                }
            }
        }
        blocks.push((sample.offset, frames));
        total = total.checked_add(frames).ok_or_else(range)?;
    }
    let (window, exact) = match verdict {
        Err(error) => (Err(error), true),
        Ok(()) => match exact_ticks_per_unit(format.rate) {
            None => (
                Err(ReelError::Unsupported {
                    what: "sample-rate-not-tick-exact",
                }),
                true,
            ),
            Some(ticks_per_frame) => {
                let start = media_to_ticks(first_pts, units_per_second)?;
                (
                    Ok(PcmWindow {
                        presentation_start: start.value,
                        first_frame: 0,
                        frames: total,
                        ticks_per_frame,
                    }),
                    start.exact,
                )
            }
        },
    };
    PcmStream::from_blocks(format, blocks, window, exact).map(Some)
}
