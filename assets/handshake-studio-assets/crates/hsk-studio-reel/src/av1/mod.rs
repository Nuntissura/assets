//! AV1 video decode behind the optional cargo feature `av1` (default OFF): the IN-PROCESS decoder.
//!
//! Decoder: rav1d 1.1.0 (BSD-2-Clause, Rust port of dav1d) built with its `asm` feature OFF (pure Rust,
//! no nasm). Pinned and single-threaded: AV1 decoding is normative, so the output is bit-exact with any
//! conformant decoder (libdav1d, libaom, hardware) and hash-testable; every frame and every segment
//! receipt carries the decoder id and version. ISOBMFF `av01` (config `av1C`) and Matroska `V_AV1`
//! (CodecPrivate = `av1C`) packets are decoded through the containers' own providers
//! (`OutputMode::Decoded` -> `ChunkKind::VideoFrame` chunks laid out as `VideoFormat` says) when the
//! caller marks the source `SourceTrust::Trusted` (see `crate::video` for the routing policy).
//!
//! Colour: ITU-T H.273 code points come from the sequence header when it carries a colour description,
//! else from the container (`colr` nclx), else stay "unspecified" (2) and are tagged `Untagged`; the range
//! flag always comes from the sequence header. Refused with typed errors: `av1C` version/profile this
//! build does not define (`Unsupported`), reserved bit depths, malformed streams (`Corrupt`), frames
//! beyond the limits (`LimitExceeded`), content-encoded Matroska tracks (`Unsupported`).
//!
//! TRUSTED INPUT ONLY. rav1d 1.1.0 still contains `unwrap`/index panics reachable from corrupted
//! bitstreams (observed: `decode.rs:4997` `Option::unwrap()` on a byte-mutated fixture). rav1d exposes the
//! decoder as `extern "C"` functions, and a panic inside one aborts the whole process (it cannot be caught
//! with `catch_unwind`). Untrusted media is therefore never routed here: the default route sends it to an
//! external (isolated) decoder, e.g. `hsk-studio-reel-native`'s worker process, or refuses it with
//! `CodecUnavailable`. `Av1Decoder` stays public because that worker hosts it.
#![deny(clippy::arithmetic_side_effects)]
mod ffi;

use crate::av1_config::av1_descriptor;
use crate::error::ReelError;
use crate::isobmff::ColorTag;
use crate::model::*;
use crate::video::{DecodedPicture, PictureSink, VideoDecodeError, VideoDecodeSession};
use ffi::{Layout, Rav1d, RawPicture, SendStatus};
use hsk_studio_accord::CancellationToken;
use std::collections::VecDeque;

// The record parser is pure and always compiled; this path stays valid for in-process users.
pub use crate::av1_config::{Av1Config, parse_av1c};

pub const DECODER_ID: &str = "rav1d";

/// Version of the linked rav1d (read from the crate, not hard-coded).
pub fn decoder_version() -> &'static str {
    ffi::version()
}

/// Enumerable capability descriptor (STU-VID-051) for AV1 decode by the in-process decoder.
pub fn descriptor() -> CodecDescriptor {
    av1_descriptor(decoder_version(), true)
}

/// One decoded frame with its own receipt fields (decoder id + version).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedFrame {
    /// The tag passed with the packet this frame came from.
    pub tag: u64,
    pub format: VideoFormat,
    pub film_grain_applied: bool,
    pub decoder_id: &'static str,
    pub decoder_version: &'static str,
    /// Y, U, V planes (Y only for mono), tight rows; see `VideoFormat`.
    pub planes: Vec<Vec<u8>>,
}

impl DecodedFrame {
    /// Append the packed frame (planes concatenated in delivery order).
    pub fn pack_into(&self, out: &mut Vec<u8>) {
        for plane in &self.planes {
            out.extend_from_slice(plane);
        }
    }
}

fn frame_color(sequence: ffi::SequenceColor, container: ColorTag) -> ColorInfo {
    if sequence.described {
        return ColorInfo {
            primaries: sequence.primaries,
            transfer: sequence.transfer,
            matrix: sequence.matrix,
            full_range: sequence.full_range,
            origin: ColorOrigin::SequenceHeader,
        };
    }
    match container {
        ColorTag::Nclx {
            primaries,
            transfer,
            matrix,
            ..
        } => ColorInfo {
            primaries,
            transfer,
            matrix,
            full_range: sequence.full_range,
            origin: ColorOrigin::Container,
        },
        _ => ColorInfo {
            primaries: 2,
            transfer: 2,
            matrix: 2,
            full_range: sequence.full_range,
            origin: ColorOrigin::Untagged,
        },
    }
}

fn frame_from(picture: RawPicture, container: ColorTag) -> DecodedFrame {
    DecodedFrame {
        tag: picture.tag,
        format: VideoFormat {
            width: picture.width,
            height: picture.height,
            bit_depth: picture.bit_depth,
            chroma: match picture.layout {
                Layout::Mono => ChromaFormat::Mono,
                Layout::I420 => ChromaFormat::Yuv420,
                Layout::I422 => ChromaFormat::Yuv422,
                Layout::I444 => ChromaFormat::Yuv444,
            },
            color: frame_color(picture.color, container),
        },
        film_grain_applied: picture.film_grain_applied,
        decoder_id: DECODER_ID,
        decoder_version: decoder_version(),
        planes: picture.planes,
    }
}

/// Safe AV1 decoder: `send` temporal units in decode order, `next_frame` in presentation order.
pub struct Av1Decoder {
    raw: Rav1d,
    container_color: ColorTag,
    ready: VecDeque<DecodedFrame>,
}

/// Bounded retries when the decoder reports a pending picture before accepting more data.
const MAX_SEND_ATTEMPTS: u32 = 8;

impl Av1Decoder {
    /// `config` is the `av1C` record (or empty); its configuration OBUs are fed first.
    pub fn open(
        limits: &Limits,
        config: &[u8],
        container_color: ColorTag,
    ) -> Result<Self, ReelError> {
        let parsed = if config.is_empty() {
            None
        } else {
            Some(parse_av1c(config)?)
        };
        // Largest picture whose packed form still fits one chunk: sized from the stream's own `av1C`
        // layout and bit depth (1080p 4:2:0 8-bit fits the default chunk). Only a stream without a
        // record falls back to the 4:4:4 16-bit worst case.
        let pixel_limit = parsed.as_ref().map_or_else(
            || u32::try_from(limits.max_chunk_bytes / 6).unwrap_or(u32::MAX),
            |config| config.pixel_limit(limits.max_chunk_bytes),
        );
        let mut decoder = Self {
            raw: Rav1d::open(pixel_limit, limits.max_chunk_bytes)?,
            container_color,
            ready: VecDeque::new(),
        };
        if let Some(parsed) = parsed {
            decoder.send(&parsed.config_obus, u64::MAX)?;
        }
        Ok(decoder)
    }

    /// Feed one temporal unit (AV1 low-overhead OBU format). `tag` is returned on its frame.
    pub fn send(&mut self, unit: &[u8], tag: u64) -> Result<(), ReelError> {
        if unit.is_empty() {
            return Ok(());
        }
        for _ in 0..MAX_SEND_ATTEMPTS {
            match self.raw.send_data(unit, tag)? {
                SendStatus::Accepted => return self.pull(),
                SendStatus::Full => self.pull()?,
            }
        }
        Err(ReelError::Corrupt {
            what: "av1-decoder-stalled",
        })
    }

    /// Drain pictures still held by the decoder after the last unit.
    pub fn finish(&mut self) -> Result<(), ReelError> {
        self.pull()
    }

    pub fn next_frame(&mut self) -> Option<DecodedFrame> {
        self.ready.pop_front()
    }

    fn pull(&mut self) -> Result<(), ReelError> {
        while let Some(picture) = self.raw.get_picture()? {
            self.ready
                .push_back(frame_from(picture, self.container_color));
        }
        Ok(())
    }
}

/// The in-process decoder as a routed decode session.
struct InProcessSession(Av1Decoder);

impl InProcessSession {
    fn drain(&mut self, out: &mut dyn PictureSink) -> Result<(), VideoDecodeError> {
        while let Some(frame) = self.0.next_frame() {
            out.accept(DecodedPicture {
                tag: frame.tag,
                format: frame.format,
                film_grain_applied: frame.film_grain_applied,
                planes: frame.planes,
            })?;
        }
        Ok(())
    }
}

impl VideoDecodeSession for InProcessSession {
    fn send(
        &mut self,
        packet: &[u8],
        tag: u64,
        _cancel: &CancellationToken,
        out: &mut dyn PictureSink,
    ) -> Result<(), VideoDecodeError> {
        self.0.send(packet, tag)?;
        self.drain(out)
    }

    fn finish(
        &mut self,
        _cancel: &CancellationToken,
        out: &mut dyn PictureSink,
    ) -> Result<(), VideoDecodeError> {
        self.0.finish()?;
        self.drain(out)
    }
}

/// Open the in-process decoder for one segment (the route calls this only for trusted sources).
pub(crate) fn open_session(
    limits: &Limits,
    config: &[u8],
    container_color: ColorTag,
) -> Result<Box<dyn VideoDecodeSession>, ReelError> {
    Ok(Box::new(InProcessSession(Av1Decoder::open(
        limits,
        config,
        container_color,
    )?)))
}
