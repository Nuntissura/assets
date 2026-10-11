//! AV1 video decode behind the optional cargo feature `av1` (default OFF).
//!
//! Decoder: rav1d 1.1.0 (BSD-2-Clause, Rust port of dav1d) built with its `asm` feature OFF (pure Rust,
//! no nasm). Pinned and single-threaded: AV1 decoding is normative, so the output is bit-exact with any
//! conformant decoder (libdav1d, libaom, hardware) and hash-testable; every frame and every segment
//! receipt carries the decoder id and version. ISOBMFF `av01` (config `av1C`) and Matroska `V_AV1`
//! (CodecPrivate = `av1C`) packets are decoded through the containers' own providers
//! (`OutputMode::Decoded` -> `ChunkKind::VideoFrame` chunks laid out as `VideoFormat` says).
//!
//! Colour: ITU-T H.273 code points come from the sequence header when it carries a colour description,
//! else from the container (`colr` nclx), else stay "unspecified" (2) and are tagged `Untagged`; the range
//! flag always comes from the sequence header. Refused with typed errors: `av1C` version/profile this
//! build does not define (`Unsupported`), reserved bit depths, malformed streams (`Corrupt`), frames
//! beyond the limits (`LimitExceeded`), content-encoded Matroska tracks (`Unsupported`).
mod ffi;

use crate::error::{ReelError, check_cancel};
use crate::isobmff::{ColorTag, TrackIndex, deliver_packets};
use crate::model::*;
use ffi::{Layout, Rav1d, RawPicture, SendStatus};
use hsk_studio_accord::{CancellationToken, Ticks};
use std::collections::VecDeque;

pub const DECODER_ID: &str = "rav1d";

/// Version of the linked rav1d (read from the crate, not hard-coded).
pub fn decoder_version() -> &'static str {
    ffi::version()
}

/// Enumerable capability descriptor (STU-VID-051) for AV1 decode.
pub fn descriptor() -> CodecDescriptor {
    CodecDescriptor {
        id: "av1",
        kind: MediaKind::Video,
        container: "isobmff-mp4|matroska-webm",
        bit_depths: &[8, 10, 12],
        chroma: Some("mono|420|422|444"),
        transfer: None,
        decoder_version: decoder_version(),
        can_decode: true,
        can_encode: false,
    }
}

/// `AV1CodecConfigurationRecord` (`av1C` box payload / Matroska CodecPrivate).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Av1Config {
    pub profile: u8,
    pub level: u8,
    pub high_bitdepth: bool,
    pub twelve_bit: bool,
    pub monochrome: bool,
    pub subsampling_x: bool,
    pub subsampling_y: bool,
    /// Sequence header (and other) OBUs, fed to the decoder before the first packet.
    pub config_obus: Vec<u8>,
}

pub fn parse_av1c(record: &[u8]) -> Result<Av1Config, ReelError> {
    let [b0, b1, b2, _, ..] = *record else {
        return Err(ReelError::Corrupt {
            what: "av1c-truncated",
        });
    };
    if b0 & 0x80 == 0 {
        return Err(ReelError::Corrupt {
            what: "av1c-marker",
        });
    }
    if b0 & 0x7F != 1 {
        return Err(ReelError::Unsupported {
            what: "av1c-version",
        });
    }
    let profile = b1 >> 5;
    if profile > 2 {
        return Err(ReelError::Unsupported {
            what: "av1-profile",
        });
    }
    let (high_bitdepth, twelve_bit) = (b2 & 0x40 != 0, b2 & 0x20 != 0);
    // Profile 2 alone defines 12-bit samples; profiles 0 and 1 are 8 or 10 bit.
    if twelve_bit && (!high_bitdepth || profile != 2) {
        return Err(ReelError::Corrupt {
            what: "av1c-bit-depth",
        });
    }
    Ok(Av1Config {
        profile,
        level: b1 & 0x1F,
        high_bitdepth,
        twelve_bit,
        monochrome: b2 & 0x10 != 0,
        subsampling_x: b2 & 0x08 != 0,
        subsampling_y: b2 & 0x04 != 0,
        config_obus: record[4..].to_vec(),
    })
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
        // Largest picture whose packed 4:4:4 16-bit form still fits one chunk.
        let pixel_limit = u32::try_from(limits.max_chunk_bytes / 6).unwrap_or(u32::MAX);
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

#[derive(Debug, Clone, Copy)]
struct PacketMeta {
    index: u64,
    pts: i64,
    duration: Ticks,
    sync: bool,
    preroll: bool,
    trim_start: u64,
}

/// Chunk sink that decodes packets and forwards the frames of the requested span.
struct Pump<'a> {
    decoder: Av1Decoder,
    pending: VecDeque<PacketMeta>,
    sink: &'a mut dyn ChunkSink,
    cancel: &'a CancellationToken,
    max_chunk_bytes: usize,
    format: Option<VideoFormat>,
    delivered: u64,
    packed: Vec<u8>,
}

impl Pump<'_> {
    fn forward_ready(&mut self) -> Result<(), ReelError> {
        while let Some(frame) = self.decoder.next_frame() {
            let Some(at) = self.pending.iter().position(|m| m.index == frame.tag) else {
                continue; // configuration units and dropped packets carry no presentable frame
            };
            let meta = self.pending[at];
            self.pending.drain(..=at);
            if meta.preroll {
                continue; // decoded only to reach the requested frames
            }
            match self.format {
                None => self.format = Some(frame.format),
                Some(first) if first != frame.format => {
                    return Err(ReelError::Unsupported {
                        what: "av1-format-change-mid-segment",
                    });
                }
                Some(_) => {}
            }
            self.packed.clear();
            frame.pack_into(&mut self.packed);
            if self.packed.len() > self.max_chunk_bytes {
                return Err(ReelError::LimitExceeded {
                    what: "av1-frame-bytes",
                });
            }
            self.sink.accept(Chunk {
                kind: ChunkKind::VideoFrame,
                pts: meta.pts,
                duration: meta.duration,
                index: meta.index,
                units: 1,
                sync: meta.sync,
                preroll: false,
                trim_start: meta.trim_start,
                bytes: &self.packed,
            })?;
            self.delivered += 1;
        }
        Ok(())
    }
}

impl ChunkSink for Pump<'_> {
    fn accept(&mut self, chunk: Chunk<'_>) -> Result<(), ReelError> {
        check_cancel(self.cancel)?;
        self.pending.push_back(PacketMeta {
            index: chunk.index,
            pts: chunk.pts,
            duration: chunk.duration,
            sync: chunk.sync,
            preroll: chunk.preroll,
            trim_start: chunk.trim_start,
        });
        self.decoder.send(chunk.bytes, chunk.index)?;
        self.forward_ready()
    }
}

/// Decode one AV1 track segment: the container's packet delivery (frame-accurate start, pre-roll from the
/// nearest earlier sync sample) feeds the decoder; frames of the requested span reach `sink` as
/// `ChunkKind::VideoFrame` chunks. The receipt names rav1d and its version and carries the frame layout.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decode_video_track(
    src: &mut dyn ReadSeek,
    track: &TrackIndex,
    config: &[u8],
    container_color: ColorTag,
    codec_label: String,
    demux_id: &'static str,
    req: &SegmentRequest,
    limits: &Limits,
    cancel: &CancellationToken,
    sink: &mut dyn ChunkSink,
) -> Result<SegmentReceipt, ReelError> {
    let mut pump = Pump {
        decoder: Av1Decoder::open(limits, config, container_color)?,
        pending: VecDeque::new(),
        sink,
        cancel,
        max_chunk_bytes: limits.max_chunk_bytes,
        format: None,
        delivered: 0,
        packed: Vec::new(),
    };
    let mut receipt = deliver_packets(
        src,
        track,
        codec_label,
        demux_id,
        req,
        limits,
        cancel,
        &mut pump,
    )?;
    pump.decoder.finish()?;
    pump.forward_ready()?;
    receipt.decoder_id = DECODER_ID;
    receipt.decoder_version = decoder_version();
    receipt.video = pump.format;
    receipt.actual.units = pump.delivered;
    if pump.delivered < req.count {
        receipt.disposition = Disposition::Partial;
        receipt.complete = false;
    }
    Ok(receipt)
}
