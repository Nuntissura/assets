//! External (isolated) audio decoders (lane SMB-D24A-CODEC-DEFAULTS, Operator decision D24-A): AAC decode is
//! on by default THROUGH THE OS DECODER, the Windows Media Foundation AAC decoder run in reel-native's
//! isolated worker (the pattern D24 uses for H.264). Reel only declares the port; the embedder registers an
//! implementation (`Registry::with_audio_decoder`, `VideoRoute::with_audio_decoder`,
//! `decode_audio_file_with`). Studio's own AAC-LC decoder is opt-in behind cargo feature `studio-aac`.
//!
//! Routing for AAC-LC (ADTS files and `mp4a`/`A_AAC` tracks): the registered external decoder when it
//! supports the codec; else Studio's decoder when feature `studio-aac` is compiled in; else the typed
//! `MEDIA_CODEC_UNAVAILABLE aac-lc`.
//!
//! The decoder is untrusted output from reel's point of view: every block it returns is checked here before
//! it reaches the delivery loop (`Checked`): whole frames of the declared channel count, at most
//! `MAX_EXTERNAL_BLOCK_FRAMES` frames per access unit, never more frames in total than
//! `FRAMES_PER_ACCESS_UNIT` per access unit sent, and finite samples within `EXTERNAL_OUTPUT_BOUND`; anything
//! else is `MEDIA_DECODER_FAULT decoder_protocol`.
#![deny(clippy::arithmetic_side_effects)]
use super::packet::PacketDecoder;
use crate::error::{DecoderFault, ReelError};
use crate::model::{Certification, CodecDescriptor, Limits, MediaKind};
use crate::video::DecoderIdentity;
use hsk_studio_accord::CancellationToken;

/// Decoder id of the OS AAC decoder (receipts, HELLO of reel-native's worker).
pub const AAC_OS_DECODER_ID: &str = "mf-aac";
/// Licence statement of the OS AAC decoder.
pub const AAC_OS_LICENCE: &str = "Windows Media Foundation AAC decoder, OS-licensed";
/// User-facing name of the OS AAC adapter.
pub const AAC_OS_DISPLAY_NAME: &str = "AAC-LC (Windows Media Foundation AAC decoder, OS-licensed)";
/// Decoded frames one AAC-LC access unit carries (ISO/IEC 14496-3, 1024-sample frames; 960 is refused).
pub const FRAMES_PER_ACCESS_UNIT: usize = 1024;
/// Most frames one external decode call may return (one access unit plus one held back).
pub const MAX_EXTERNAL_BLOCK_FRAMES: usize = 2 * FRAMES_PER_ACCESS_UNIT;
/// Largest magnitude an external decoder's sample may have (same bound as the MPEG audio decoder's).
pub const EXTERNAL_OUTPUT_BOUND: f32 = 8.0;

/// Compressed audio codecs a route can hand to an external decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AudioCodec {
    /// AAC-LC (object type 2), 1024-sample frames, channel configurations 1-7.
    AacLc,
}

impl AudioCodec {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AacLc => "aac-lc",
        }
    }
}

/// What an external audio decoder is told about the stream, checked by reel before `open`.
#[derive(Debug, Clone)]
pub struct AudioStreamSpec<'a> {
    pub codec: AudioCodec,
    /// AudioSpecificConfig (ISO/IEC 14496-3 1.6.2.1), already parsed and accepted by reel.
    pub config: &'a [u8],
    pub sample_rate: u32,
    pub channels: u16,
    /// Channel configuration of the AudioSpecificConfig (1-7): the decoder delivers channels in that
    /// configuration's element order (ISO/IEC 14496-3 channel configuration table), like Studio's decoder.
    pub channel_config: u8,
    pub limits: Limits,
}

/// One open decode of one stream. Access units arrive in decode order, without container framing (no ADTS
/// header). Each call appends interleaved f32 samples (nominally -1..=1) in the stream's channel order.
pub trait AudioDecodeSession: Send {
    /// Decode one access unit; returns the frames (samples per channel) appended to `out`.
    fn decode(
        &mut self,
        access_unit: &[u8],
        cancel: &CancellationToken,
        out: &mut Vec<f32>,
    ) -> Result<usize, ReelError>;
    /// No more access units: append what the decoder still holds; returns the frames appended.
    fn finish(&mut self, cancel: &CancellationToken, out: &mut Vec<f32>) -> Result<usize, ReelError> {
        let _ = (cancel, out);
        Ok(0)
    }
}

/// A decoder that lives outside the caller's process (reel-native's Media Foundation worker).
pub trait ExternalAudioDecoder: Send + Sync {
    fn identity(&self) -> DecoderIdentity;
    fn supports(&self, codec: AudioCodec) -> bool;
    fn open(
        &self,
        spec: &AudioStreamSpec<'_>,
        cancel: &CancellationToken,
    ) -> Result<Box<dyn AudioDecodeSession>, ReelError>;
}

/// Capability descriptor of AAC-LC decode by the OS decoder reporting `decoder_version` (STU-VID-051 as
/// enriched by D23: decoder id, version, licence, certification).
pub fn aac_os_descriptor(decoder_version: &'static str, can_decode: bool) -> CodecDescriptor {
    CodecDescriptor {
        id: "aac-lc",
        name: AAC_OS_DISPLAY_NAME,
        kind: MediaKind::Audio,
        container: "adts|isobmff|matroska",
        bit_depths: &[32],
        chroma: None,
        transfer: None,
        decoder_id: AAC_OS_DECODER_ID,
        decoder_version,
        licence: AAC_OS_LICENCE,
        certification: Certification::NotApplicable,
        can_decode,
        can_encode: false,
    }
}

fn protocol() -> ReelError {
    ReelError::DecoderFault {
        fault: DecoderFault::Protocol,
    }
}

/// An external session behind reel's checks, as the delivery loop's `PacketDecoder`.
pub(crate) struct Checked {
    session: Box<dyn AudioDecodeSession>,
    cancel: CancellationToken,
    channels: usize,
    /// Access units sent and frames received so far (frames never exceed units * 1024).
    units: u64,
    frames: u64,
    finished: bool,
}

impl Checked {
    pub(crate) fn open(
        decoder: &dyn ExternalAudioDecoder,
        spec: &AudioStreamSpec<'_>,
        cancel: &CancellationToken,
    ) -> Result<Self, ReelError> {
        Ok(Self {
            session: decoder.open(spec, cancel)?,
            cancel: cancel.clone(),
            channels: usize::from(spec.channels),
            units: 0,
            frames: 0,
            finished: false,
        })
    }

    /// Checks the block the session appended after `before`; returns its frames.
    fn check(&mut self, out: &mut Vec<f32>, before: usize, claimed: usize) -> Result<usize, ReelError> {
        let added = out.len().checked_sub(before).ok_or_else(protocol)?;
        let frames = added.checked_div(self.channels).ok_or_else(protocol)?;
        let whole = frames.checked_mul(self.channels) == Some(added);
        let total = self
            .frames
            .checked_add(frames as u64)
            .filter(|t| Some(*t) <= self.units.checked_mul(FRAMES_PER_ACCESS_UNIT as u64));
        let finite = out
            .get(before..)
            .unwrap_or(&[])
            .iter()
            .all(|v| v.is_finite() && v.abs() <= EXTERNAL_OUTPUT_BOUND);
        match total {
            Some(total) if whole && claimed == frames && frames <= MAX_EXTERNAL_BLOCK_FRAMES && finite => {
                self.frames = total;
                Ok(frames)
            }
            _ => {
                out.truncate(before);
                Err(protocol())
            }
        }
    }
}

impl PacketDecoder for Checked {
    fn decode(&mut self, packet: &[u8], out: &mut Vec<f32>) -> Result<usize, ReelError> {
        if self.finished {
            return Err(protocol());
        }
        self.units = self.units.saturating_add(1);
        let before = out.len();
        let claimed = self.session.decode(packet, &self.cancel, out)?;
        self.check(out, before, claimed)
    }

    fn flush(&mut self, out: &mut Vec<f32>) -> Result<usize, ReelError> {
        if self.finished {
            return Ok(0);
        }
        self.finished = true;
        let before = out.len();
        let claimed = self.session.finish(&self.cancel, out)?;
        self.check(out, before, claimed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lying external session: returns whatever the test scripted.
    struct Script(Vec<(Vec<f32>, usize)>);
    impl AudioDecodeSession for Script {
        fn decode(&mut self, _: &[u8], _: &CancellationToken, out: &mut Vec<f32>) -> Result<usize, ReelError> {
            let (block, claimed) = self.0.remove(0);
            out.extend_from_slice(&block);
            Ok(claimed)
        }
    }

    fn checked(script: Vec<(Vec<f32>, usize)>) -> Checked {
        Checked {
            session: Box::new(Script(script)),
            cancel: CancellationToken::default(),
            channels: 2,
            units: 0,
            frames: 0,
            finished: false,
        }
    }

    /// Hostile external output is a decoder protocol fault and nothing of it stays in `out`: a block that
    /// is not whole frames, a frame count other than the claimed one, more frames than access units allow,
    /// a non-finite or out-of-bound sample; an honest block passes.
    #[test]
    fn external_audio_output_is_checked() {
        let mut out = vec![0.5];
        assert_eq!(checked(vec![(vec![0.0; 2048], 1024)]).decode(&[1], &mut out), Ok(1024));
        for (block, claimed) in [
            (vec![0.0; 3], 1),
            (vec![0.0; 2048], 1023),
            (vec![0.0; 2 * 1025], 1025),
            (vec![f32::NAN, 0.0], 1),
            (vec![9.0, 0.0], 1),
        ] {
            let mut out = vec![0.5];
            assert_eq!(
                checked(vec![(block, claimed)]).decode(&[1], &mut out),
                Err(protocol())
            );
            assert_eq!(out, [0.5]);
        }
        // A decoder used after finish is a protocol fault; finish itself runs once.
        let mut c = checked(Vec::new());
        assert_eq!(c.flush(&mut Vec::new()), Ok(0));
        assert_eq!(c.flush(&mut Vec::new()), Ok(0));
        assert_eq!(c.decode(&[1], &mut Vec::new()), Err(protocol()));
    }
}
