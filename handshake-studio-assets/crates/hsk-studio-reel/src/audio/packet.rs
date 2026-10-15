//! Packet-codec glue: a `PacketSource` (container demux) feeding a `PacketDecoder` (codec) is a
//! `FrameSource` for the shared delivery loop. Third-party decoders run behind `guarded`, which turns a
//! panic into the typed `MEDIA_DECODER_FAULT decoder_crashed` and poisons the decoder.
#![deny(clippy::arithmetic_side_effects)]
use super::engine::FrameSource;
use crate::error::{DecoderFault, ReelError};
use hsk_studio_accord::CancellationToken;

/// Compressed packets of one audio stream in decode order.
pub(crate) trait PacketSource {
    fn next_packet(&mut self, cancel: &CancellationToken) -> Result<Option<Vec<u8>>, ReelError>;
    fn bytes_read(&self) -> u64;
}

/// One codec instance.
pub(crate) trait PacketDecoder {
    /// Decode one packet and append its interleaved samples to `out`; returns the frames appended.
    fn decode(&mut self, packet: &[u8], out: &mut Vec<f32>) -> Result<usize, ReelError>;
}

impl<T: PacketDecoder + ?Sized> PacketDecoder for Box<T> {
    fn decode(&mut self, packet: &[u8], out: &mut Vec<f32>) -> Result<usize, ReelError> {
        (**self).decode(packet, out)
    }
}

pub(crate) struct Decode<S, D> {
    pub(crate) source: S,
    pub(crate) decoder: D,
}

impl<S: PacketSource, D: PacketDecoder> FrameSource for Decode<S, D> {
    fn next_block(
        &mut self,
        out: &mut Vec<f32>,
        cancel: &CancellationToken,
    ) -> Result<Option<usize>, ReelError> {
        match self.source.next_packet(cancel)? {
            Some(packet) => self.decoder.decode(&packet, out).map(Some),
            None => Ok(None),
        }
    }

    fn bytes_read(&self) -> u64 {
        self.source.bytes_read()
    }
}

/// Run third-party decoder code; a panic becomes `DecoderFault::Crashed` and sets `poisoned`, so the
/// decoder (whose state is unknown after the panic) is never called again.
pub(crate) fn guarded<T>(
    poisoned: &mut bool,
    f: impl FnOnce() -> Result<T, ReelError>,
) -> Result<T, ReelError> {
    if *poisoned {
        return Err(ReelError::DecoderFault {
            fault: DecoderFault::Crashed,
        });
    }
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(_) => {
            *poisoned = true;
            Err(ReelError::DecoderFault {
                fault: DecoderFault::Crashed,
            })
        }
    }
}

/// Planar f32 channels -> interleaved, appended to `out`.
pub(crate) fn interleave(planar: &[Vec<f32>], out: &mut Vec<f32>) -> Result<usize, ReelError> {
    let frames = planar.first().map_or(0, Vec::len);
    if planar.iter().any(|c| c.len() != frames) {
        return Err(ReelError::Corrupt {
            what: "audio-channel-length",
        });
    }
    let total = frames
        .checked_mul(planar.len())
        .ok_or(ReelError::LimitExceeded {
            what: "audio-block",
        })?;
    out.try_reserve(total)
        .map_err(|_| ReelError::LimitExceeded {
            what: "audio-alloc",
        })?;
    for i in 0..frames {
        for ch in planar {
            out.push(ch.get(i).copied().unwrap_or(0.0));
        }
    }
    Ok(frames)
}
