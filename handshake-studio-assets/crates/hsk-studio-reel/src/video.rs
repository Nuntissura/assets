//! Compressed-video decode routing: who decodes a container's video packets, and under which trust.
//!
//! The containers (`isobmff`, `matroska`) index and deliver packets in pure Rust; decoding them is a
//! separate, optional capability with two possible hosts:
//! * an EXTERNAL decoder registered by the embedder (`ExternalVideoDecoder`: typically a worker process,
//!   see `hsk-studio-reel-native`), reachable in every build, no cargo feature needed;
//! * the IN-PROCESS decoder compiled in by cargo feature `av1` (rav1d through its C ABI).
//!
//! Policy (`SourceTrust`, default `Untrusted`): bytes the caller does not vouch for are only ever decoded
//! by the external decoder; when none is registered (or it does not support the codec) the answer is the
//! typed `CodecUnavailable`, never a silent in-process decode. The in-process decoder is used only when
//! the caller explicitly marks the source `Trusted` (and the feature is compiled in); a trusted source
//! without the feature falls back to the external decoder, else `CodecUnavailable`.
//!
//! Why: rav1d 1.1.0 aborts the whole process on a few percent of corrupted bitstreams (a panic inside an
//! `extern "C"` function cannot be caught), so untrusted media must be decoded behind a process boundary.
//!
//! Both hosts run through ONE packet pump (`decode_video_track`): frame-accurate start and pre-roll come
//! from the container, pictures are matched to packets by tag, pre-roll pictures are dropped, a mid-segment
//! format change, any picture whose planes disagree with its declared format and any picture larger than
//! the size the container declares for the track (`within_declared_size`: the declared size is the
//! sequence maximum) are refused before the picture is packed, and the receipt names the decoder that
//! actually ran (`DecoderIdentity`).
#![deny(clippy::arithmetic_side_effects)]
use crate::av1_config::parse_av1c;
use crate::error::{DecoderFault, ReelError, check_cancel};
use crate::isobmff::{ColorTag, TrackIndex, deliver_packets};
use crate::mkv_content::{ContentPlan, DecodeSink};
use crate::model::*;
use hsk_studio_accord::{CancellationToken, Ticks};
use std::collections::VecDeque;

/// How far the caller vouches for the bytes of a source.
///
/// Trust belongs to the bytes that are decoded, not to a request: a proxy (or any other derived source)
/// is a different source and carries its own trust (`ProxySource::trust`, default `Untrusted`); the
/// trust stated for an original never extends to it (test `trusted_original_never_lends_its_trust_to_a_proxy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SourceTrust {
    /// Bytes from anywhere the caller does not control (imports, downloads, user media). The default:
    /// decoded only by the external decoder.
    #[default]
    Untrusted,
    /// The caller vouches for the bytes (produced by itself, or checked against a known-good digest):
    /// the in-process decoder may run them.
    Trusted,
}

/// Compressed video codecs a route can hand to a decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum VideoCodec {
    Av1,
}

impl VideoCodec {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Av1 => "av1",
        }
    }
}

/// Identity of a decoder, copied into every receipt it produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecoderIdentity {
    pub id: &'static str,
    pub version: &'static str,
}

/// What an external decoder is told about the stream it will decode.
#[derive(Debug, Clone)]
pub struct VideoStreamSpec<'a> {
    pub codec: VideoCodec,
    /// `"isobmff"` or `"matroska"`.
    pub container: &'static str,
    /// Codec configuration record (`av1C` payload / Matroska CodecPrivate), already validated; may be empty.
    pub config: &'a [u8],
    /// Colour tag the container carried for the stream.
    pub color: ColorTag,
    /// Picture size the container DECLARES (0 when it declares none; the AV1 sequence maximum, so a smaller
    /// picture is legal): a decoder should refuse pictures above it instead of allocating whatever the
    /// bitstream claims. The route does not rely on that: a picture above it is a
    /// `DecoderFault::Protocol` and is not copied (test `decoded_picture_size_is_checked_against_the_container_declared_size`).
    pub width: u32,
    pub height: u32,
    pub limits: Limits,
}

/// One decoded picture: planes Y, U, V (Y only for mono), tight rows, as `VideoFormat` documents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedPicture {
    /// The tag passed with the packet this picture came from.
    pub tag: u64,
    pub format: VideoFormat,
    pub film_grain_applied: bool,
    pub planes: Vec<Vec<u8>>,
}

/// Receives the pictures a session releases, one at a time (no batch is ever buffered by the route).
pub trait PictureSink {
    fn accept(&mut self, picture: DecodedPicture) -> Result<(), ReelError>;
}

/// Failure of a decode session: a statement about the media, or a fault of the decoder itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoDecodeError {
    Media(ReelError),
    Fault(DecoderFault),
}

impl VideoDecodeError {
    /// The route's mapping: media errors pass through unchanged, decoder faults become the distinct
    /// `ReelError::DecoderFault`.
    pub fn into_reel(self) -> ReelError {
        match self {
            Self::Media(error) => error,
            Self::Fault(fault) => ReelError::DecoderFault { fault },
        }
    }
}

impl From<ReelError> for VideoDecodeError {
    fn from(error: ReelError) -> Self {
        Self::Media(error)
    }
}

impl From<DecoderFault> for VideoDecodeError {
    fn from(fault: DecoderFault) -> Self {
        Self::Fault(fault)
    }
}

/// One open decode of one segment. Packets arrive in decode order; pictures leave through `out` in the
/// order the decoder releases them (each carrying the tag of its packet).
pub trait VideoDecodeSession {
    fn send(
        &mut self,
        packet: &[u8],
        tag: u64,
        cancel: &CancellationToken,
        out: &mut dyn PictureSink,
    ) -> Result<(), VideoDecodeError>;
    /// No more packets: release every picture still held.
    fn finish(
        &mut self,
        cancel: &CancellationToken,
        out: &mut dyn PictureSink,
    ) -> Result<(), VideoDecodeError>;
}

/// A decoder that lives outside the caller's process (or otherwise behind the embedder's own isolation).
/// Registered on a `Registry` (`with_video_decoder`) or passed in a `VideoRoute`.
pub trait ExternalVideoDecoder: Send + Sync {
    fn identity(&self) -> DecoderIdentity;
    fn supports(&self, codec: VideoCodec) -> bool;
    fn open(
        &self,
        spec: &VideoStreamSpec<'_>,
        cancel: &CancellationToken,
    ) -> Result<Box<dyn VideoDecodeSession>, VideoDecodeError>;
}

/// The routing inputs of one decode call.
#[derive(Clone, Copy)]
pub struct VideoRoute<'a> {
    pub trust: SourceTrust,
    pub external: Option<&'a dyn ExternalVideoDecoder>,
}

impl Default for VideoRoute<'_> {
    /// Untrusted source, no external decoder: compressed video is `CodecUnavailable`.
    fn default() -> Self {
        Self {
            trust: SourceTrust::Untrusted,
            external: None,
        }
    }
}

impl<'a> VideoRoute<'a> {
    pub const fn untrusted() -> Self {
        Self {
            trust: SourceTrust::Untrusted,
            external: None,
        }
    }

    /// The caller vouches for the source: the in-process decoder (feature `av1`) may run it.
    pub const fn trusted() -> Self {
        Self {
            trust: SourceTrust::Trusted,
            external: None,
        }
    }

    pub const fn with_external(mut self, decoder: &'a dyn ExternalVideoDecoder) -> Self {
        self.external = Some(decoder);
        self
    }
}

impl std::fmt::Debug for VideoRoute<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoRoute")
            .field("trust", &self.trust)
            .field("external", &self.external.map(|e| e.identity()))
            .finish()
    }
}

enum Chosen<'a> {
    #[cfg(feature = "av1")]
    InProcess,
    External(&'a dyn ExternalVideoDecoder),
}

/// The policy in one place: trusted + compiled in -> in-process; otherwise the external decoder when it
/// supports the codec; otherwise nobody.
fn choose<'a>(route: &VideoRoute<'a>, codec: VideoCodec) -> Option<Chosen<'a>> {
    #[cfg(feature = "av1")]
    if route.trust == SourceTrust::Trusted && codec == VideoCodec::Av1 {
        return Some(Chosen::InProcess);
    }
    route
        .external
        .filter(|external| external.supports(codec))
        .map(Chosen::External)
}

/// A container track that carries compressed video.
pub(crate) struct VideoTrack<'a> {
    pub track: &'a TrackIndex,
    pub codec: VideoCodec,
    pub container: &'static str,
    pub demux_id: &'static str,
    pub config: &'a [u8],
    pub color: ColorTag,
    /// Codec name for receipts and for `CodecUnavailable` (`av01`, `V_AV1`).
    pub codec_label: String,
    pub width: u32,
    pub height: u32,
    /// Container feature this track uses that the route cannot undo; refused once a decoder exists.
    pub unsupported: Option<&'static str>,
}

/// Container-side content decoding of a track (Matroska `ContentEncodings`): the plan that turns stored
/// packets into codec packets, and a refusal (encryption, ...) that is reported once a decoder exists.
#[derive(Default)]
pub(crate) struct VideoContent<'a> {
    pub plan: Option<&'a ContentPlan>,
    pub refusal: Option<ReelError>,
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

/// Forwards pictures to the caller's sink: a picture belongs to the packet whose tag it carries, pre-roll
/// pictures are dropped, the first delivered format must not change inside one segment, and a picture
/// whose planes disagree with its declared format, or whose size exceeds the size the container declares,
/// is a decoder protocol fault. Those checks, and the packed-size check against `max_chunk_bytes`, run
/// BEFORE the picture is packed (packing copies the planes into `packed`); a refused picture is not
/// copied (test `oversize_decoded_picture_is_refused_before_it_is_copied` measures the largest allocation).
struct Forward<'a> {
    pending: VecDeque<PacketMeta>,
    sink: &'a mut dyn ChunkSink,
    max_chunk_bytes: usize,
    /// `(width, height)` the container declares for the track; 0 = that side is not declared.
    declared: (u32, u32),
    format: Option<VideoFormat>,
    delivered: u64,
    packed: Vec<u8>,
}

/// True when the picture has the plane count its format implies and every plane has exactly
/// width x height x bytes-per-sample bytes. The format comes from the decoder (untrusted output), so the
/// expected size is computed with checked arithmetic: a size that does not fit u64 can match no real
/// plane and is a mismatch (the caller reports a protocol fault), never an overflow.
fn planes_match_format(picture: &DecodedPicture) -> bool {
    let dims = picture.format.plane_dims();
    let bytes_per_sample = picture.format.bytes_per_sample() as u64;
    picture.format.width > 0
        && picture.format.height > 0
        && picture.planes.len() == dims.len()
        && picture.planes.iter().zip(dims).all(|(plane, (w, h))| {
            u64::from(w)
                .checked_mul(u64::from(h))
                .and_then(|samples| samples.checked_mul(bytes_per_sample))
                .is_some_and(|expected| plane.len() as u64 == expected)
        })
}

/// True when the picture does not exceed the size the container declares for the track. A side declared
/// as 0 is not declared and is not checked. The declared size is the SEQUENCE MAXIMUM, not the size of
/// every frame: AV1-ISOBMFF 2.2.4 sets the sample entry width/height to `max_frame_width_minus_1 + 1` /
/// `max_frame_height_minus_1 + 1`, and the Matroska `V_AV1` mapping does the same for PixelWidth and
/// PixelHeight. A picture smaller than the declared size is therefore legal; a larger one cannot come
/// from a conformant stream and is the decoder breaking the boundary.
fn within_declared_size(format: &VideoFormat, (declared_w, declared_h): (u32, u32)) -> bool {
    (declared_w == 0 || format.width <= declared_w)
        && (declared_h == 0 || format.height <= declared_h)
}

/// Bytes of the packed frame (all planes), `None` when the sum does not fit `usize`. Read from the planes
/// that exist, so it needs no allocation.
fn packed_len(picture: &DecodedPicture) -> Option<usize> {
    picture
        .planes
        .iter()
        .try_fold(0usize, |total, plane| total.checked_add(plane.len()))
}

impl PictureSink for Forward<'_> {
    fn accept(&mut self, picture: DecodedPicture) -> Result<(), ReelError> {
        let Some(at) = self.pending.iter().position(|m| m.index == picture.tag) else {
            return Ok(()); // configuration units and dropped packets carry no presentable frame
        };
        let meta = self.pending[at];
        self.pending.drain(..=at);
        if meta.preroll {
            return Ok(()); // decoded only to reach the requested frames
        }
        if !planes_match_format(&picture) || !within_declared_size(&picture.format, self.declared) {
            return Err(ReelError::DecoderFault {
                fault: DecoderFault::Protocol,
            });
        }
        match self.format {
            None => self.format = Some(picture.format),
            Some(first) if first != picture.format => {
                return Err(ReelError::Unsupported {
                    what: "av1-format-change-mid-segment",
                });
            }
            Some(_) => {}
        }
        // The chunk budget is checked on the planes that exist, before `packed` copies any of them.
        if packed_len(&picture).is_none_or(|len| len > self.max_chunk_bytes) {
            return Err(ReelError::LimitExceeded {
                what: "av1-frame-bytes",
            });
        }
        self.packed.clear();
        for plane in &picture.planes {
            self.packed.extend_from_slice(plane);
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
        self.delivered = self.delivered.saturating_add(1);
        Ok(())
    }
}

/// Chunk sink that feeds container packets to a decode session.
struct Pump<'a> {
    session: Box<dyn VideoDecodeSession>,
    fwd: Forward<'a>,
    cancel: &'a CancellationToken,
}

impl ChunkSink for Pump<'_> {
    fn accept(&mut self, chunk: Chunk<'_>) -> Result<(), ReelError> {
        check_cancel(self.cancel)?;
        self.fwd.pending.push_back(PacketMeta {
            index: chunk.index,
            pts: chunk.pts,
            duration: chunk.duration,
            sync: chunk.sync,
            preroll: chunk.preroll,
            trim_start: chunk.trim_start,
        });
        self.session
            .send(chunk.bytes, chunk.index, self.cancel, &mut self.fwd)
            .map_err(VideoDecodeError::into_reel)
    }
}

/// Decode one video track segment: the container's packet delivery (frame-accurate start, pre-roll from
/// the nearest earlier sync sample) feeds the decoder the route selects; pictures of the requested span
/// reach `sink` as `ChunkKind::VideoFrame` chunks. The receipt names the decoder that ran and carries the
/// frame layout.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decode_video_track(
    src: &mut dyn ReadSeek,
    video: VideoTrack<'_>,
    req: &SegmentRequest,
    limits: &Limits,
    cancel: &CancellationToken,
    sink: &mut dyn ChunkSink,
    route: &VideoRoute<'_>,
) -> Result<SegmentReceipt, ReelError> {
    decode_video_track_with(
        src,
        video,
        VideoContent::default(),
        req,
        limits,
        cancel,
        sink,
        route,
    )
}

/// `decode_video_track` for a track whose stored packets need the container's content decoding first.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decode_video_track_with(
    src: &mut dyn ReadSeek,
    video: VideoTrack<'_>,
    content: VideoContent<'_>,
    req: &SegmentRequest,
    limits: &Limits,
    cancel: &CancellationToken,
    sink: &mut dyn ChunkSink,
    route: &VideoRoute<'_>,
) -> Result<SegmentReceipt, ReelError> {
    let chosen = choose(route, video.codec).ok_or_else(|| ReelError::CodecUnavailable {
        codec: video.codec_label.clone(),
    })?;
    if let Some(what) = video.unsupported {
        return Err(ReelError::Unsupported { what });
    }
    if let Some(error) = content.refusal {
        return Err(error);
    }
    match video.codec {
        VideoCodec::Av1 if !video.config.is_empty() => {
            parse_av1c(video.config)?;
        }
        VideoCodec::Av1 => {}
    }
    let (session, identity): (Box<dyn VideoDecodeSession>, DecoderIdentity) = match chosen {
        #[cfg(feature = "av1")]
        Chosen::InProcess => (
            crate::av1::open_session(limits, video.config, video.color)?,
            DecoderIdentity {
                id: crate::av1::DECODER_ID,
                version: crate::av1::decoder_version(),
            },
        ),
        Chosen::External(external) => {
            let spec = VideoStreamSpec {
                codec: video.codec,
                container: video.container,
                config: video.config,
                color: video.color,
                width: video.width,
                height: video.height,
                limits: *limits,
            };
            let session = external
                .open(&spec, cancel)
                .map_err(VideoDecodeError::into_reel)?;
            (session, external.identity())
        }
    };
    let mut pump = Pump {
        session,
        fwd: Forward {
            pending: VecDeque::new(),
            sink,
            max_chunk_bytes: limits.max_chunk_bytes,
            declared: (video.width, video.height),
            format: None,
            delivered: 0,
            packed: Vec::new(),
        },
        cancel,
    };
    let mut transform;
    let packets: &mut dyn ChunkSink = match content.plan {
        Some(plan) if plan.has_frame_steps() => {
            transform = DecodeSink::new(&mut pump, plan, limits.max_chunk_bytes);
            &mut transform
        }
        _ => &mut pump,
    };
    let mut receipt = deliver_packets(
        src,
        video.track,
        video.codec_label,
        video.demux_id,
        req,
        limits,
        cancel,
        packets,
    )?;
    pump.session
        .finish(cancel, &mut pump.fwd)
        .map_err(VideoDecodeError::into_reel)?;
    receipt.decoder_id = identity.id;
    receipt.decoder_version = identity.version;
    receipt.video = pump.fwd.format;
    receipt.actual.units = pump.fwd.delivered;
    if pump.fwd.delivered < req.count {
        // A preview reports the shortfall; an export is exact or it fails. Pictures already handed to
        // the sink stay delivered: the error is the statement that the export must not publish them.
        if req.purpose == Purpose::Export {
            return Err(ReelError::Incomplete {
                requested: req.count,
                delivered: pump.fwd.delivered,
            });
        }
        receipt.disposition = Disposition::Partial;
        receipt.complete = false;
    }
    Ok(receipt)
}
