//! Host side of the Media Foundation worker modes added by Operator decision D24-A (lane
//! SMB-D24A-CODEC-DEFAULTS; feature `mf`, pure Rust, no `unsafe`), on the same pool core, supervision and
//! wire as the H.264 decoder (`host.rs`, `wire.rs`, `hsk-studio-worker`):
//!
//! * `WorkerMfAacProvider` is reel's `ExternalAudioDecoder` for AAC-LC: register it with
//!   `Registry::with_audio_decoder(Arc::new(provider))` (or pass it to `decode_audio_file_with`) and AAC of
//!   every source (ADTS files, `mp4a` / `A_AAC` tracks) is decoded by the OS AAC decoder in an isolated
//!   `reel-native-mf-worker --aac` process: AAC decode is ON by default through the OS decoder. Studio's own
//!   AAC-LC decoder stays opt-in (reel feature `studio-aac`).
//! * `WorkerMfEncoder` (feature `os-encode`, default off) is reel's `ExternalVideoEncoder`: H.264 and HEVC
//!   ENCODE only through the OS encoder MFT in `reel-native-mf-worker --encode=h264|hevc` processes. No
//!   encoder is bundled; where the OS has none (HEVC needs the paid Windows HEVC Video Extensions) the open is
//!   the typed `MEDIA_CODEC_UNAVAILABLE hevc` / `h264`.
//!
//! Every reply is checked by the pool before it is used: audio blocks must carry the declared channel count
//! and rate and the tag of a sent access unit, coded units the tag of a sent frame, at most 64 per request;
//! a breach is a counted `worker_protocol` fault and the worker is killed. Crashes, deadlines and cancel
//! behave as for the H.264 decoder (`decoder_crashed`, `decoder_deadline`, `MEDIA_CANCELED`).
#![deny(clippy::arithmetic_side_effects)]
use crate::host::{
    Av1ConfigError, Av1Error, Call, Clock, MAX_PACKET_BYTES, Payload, PayloadKind, Pool, Proc, Want,
    WorkerSpec,
};
use crate::mf_host::{MF_WORKER_PROGRAM, MfConfigError, MfWorkerOptions, MfWorkerStats};
use crate::wire::{self, OpenRequest};
use hsk_studio_accord::CancellationToken;
use hsk_studio_reel::audio::{AudioCodec, AudioDecodeSession, AudioStreamSpec, ExternalAudioDecoder};
use hsk_studio_reel::{
    AAC_OS_DECODER_ID, CodecDescriptor, ColorTag, DecoderFault, DecoderIdentity, ReelError,
    aac_os_descriptor,
};
use hsk_studio_worker::{after, resolve_program};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

/// Worker argument of the AAC mode (the worker's `AAC_ARG`).
const AAC_ARG: &str = "--aac";

pub(crate) const MF_AAC_SPEC: WorkerSpec = WorkerSpec {
    decoder_id: AAC_OS_DECODER_ID,
    owner_session: "reel-native-mf-aac",
    io_thread_name: "reel-native-mf-aac-worker-io",
    max_version_bytes: 160,
    codec_label: "aac-lc",
    session_ended: "mf-aac-session-ended",
    packet_bytes: "aac-access-unit-size",
};

fn protocol() -> ReelError {
    ReelError::DecoderFault {
        fault: DecoderFault::Protocol,
    }
}

fn check_options(options: &MfWorkerOptions) -> Result<std::path::PathBuf, MfConfigError> {
    if options.max_workers == 0
        || options.max_spawns == 0
        || options.spawn_window.is_zero()
        || options.spawn_timeout.is_zero()
        || options.batch_deadline.is_zero()
        || options.segment_deadline.is_zero()
    {
        return Err(Av1ConfigError::PolicyInvalid);
    }
    resolve_program(options.program.clone(), MF_WORKER_PROGRAM).ok_or(Av1ConfigError::WorkerProgramMissing)
}

/// One open worker session that is not a video decode: OPEN, then PACKET / FINISH batches whose payloads
/// are handed on as they arrive. Returns the worker to its pool when dropped, unless it became unusable.
pub(crate) struct RawSession {
    pool: Arc<Pool>,
    proc: Option<Proc>,
    segment_wall: Instant,
    reply_cap: usize,
    kind: PayloadKind,
    /// Tags of the PACKETs sent (bounded by the session's packets): the only tags a reply may carry.
    sent: HashSet<u64>,
}

impl Drop for RawSession {
    fn drop(&mut self) {
        if let Some(proc) = self.proc.take() {
            self.pool.checkin(proc);
        }
    }
}

impl RawSession {
    pub(crate) fn open(
        pool: &Arc<Pool>,
        open: &OpenRequest,
        reply_cap: usize,
        kind: PayloadKind,
        cancel: &CancellationToken,
    ) -> Result<Self, ReelError> {
        cancel.check().map_err(|_| ReelError::Canceled)?;
        let mut session = Self {
            pool: Arc::clone(pool),
            proc: None,
            segment_wall: after(pool.segment_deadline()),
            reply_cap: reply_cap.saturating_add(wire::REPLY_SLACK),
            kind,
            sent: HashSet::new(),
        };
        let label = pool.spec().codec_label;
        session.proc = Some(pool.checkout(cancel).map_err(|error| reel(error, label))?);
        let id = pool.next_id();
        session.call(wire::encode_open(id, open), id, Want::Opened, cancel, &mut |_| {
            Err(protocol())
        })?;
        Ok(session)
    }

    fn call(
        &mut self,
        frame: Vec<u8>,
        id: u64,
        want: Want,
        cancel: &CancellationToken,
        on: &mut dyn FnMut(Payload) -> Result<(), ReelError>,
    ) -> Result<(), ReelError> {
        let spec = self.pool.spec();
        let Some(proc) = self.proc.as_mut().filter(|proc| proc.is_usable()) else {
            return Err(ReelError::Unsupported {
                what: spec.session_ended,
            });
        };
        let call = Call {
            id,
            frame,
            cap: self.reply_cap,
            want,
            declared: (0, 0),
            sent: &self.sent,
        };
        let clock = Clock {
            cancel,
            segment_wall: self.segment_wall,
        };
        self.pool
            .exchange_any(proc, call, &clock, self.kind, on)
            .map_err(|error| reel(error, spec.codec_label))
    }

    pub(crate) fn send(
        &mut self,
        tag: u64,
        bytes: &[u8],
        cancel: &CancellationToken,
        on: &mut dyn FnMut(Payload) -> Result<(), ReelError>,
    ) -> Result<(), ReelError> {
        if bytes.is_empty() || bytes.len() > MAX_PACKET_BYTES {
            return Err(ReelError::LimitExceeded {
                what: self.pool.spec().packet_bytes,
            });
        }
        self.sent.insert(tag);
        let id = self.pool.next_id();
        self.call(wire::encode_packet(id, tag, bytes), id, Want::BatchEnd, cancel, on)
    }

    pub(crate) fn finish(
        &mut self,
        cancel: &CancellationToken,
        on: &mut dyn FnMut(Payload) -> Result<(), ReelError>,
    ) -> Result<(), ReelError> {
        let id = self.pool.next_id();
        self.call(wire::encode_finish(id), id, Want::BatchEnd, cancel, on)
    }
}

/// Reel's vocabulary for a pool failure (worker faults as for the video decoders).
fn reel(error: Av1Error, codec_label: &str) -> ReelError {
    match error {
        Av1Error::Reel(error) => error,
        Av1Error::Worker(fault) => fault.into_decode_error_for(codec_label).into_reel(),
    }
}

/// Process-isolated AAC-LC decoder through the OS (Windows Media Foundation) AAC decoder. Cheap to clone.
#[derive(Clone)]
pub struct WorkerMfAacProvider {
    pool: Arc<Pool>,
}

impl WorkerMfAacProvider {
    /// One pool of `reel-native-mf-worker --aac` processes (same pool policy as the H.264 provider; the
    /// heartbeat and DXVA settings do not apply).
    pub fn new(options: MfWorkerOptions) -> Result<Self, MfConfigError> {
        let program = check_options(&options)?;
        Ok(Self {
            pool: Arc::new(Pool::new(
                &MF_AAC_SPEC,
                program,
                vec![AAC_ARG.to_owned()],
                Vec::new(),
                &options.pool_options(),
            )),
        })
    }

    pub fn stats(&self) -> MfWorkerStats {
        self.pool.stats()
    }

    /// Capability descriptor: decoder `mf-aac`, version learned from the worker HELLO (`unknown` before the
    /// first worker started; it names the MFT, its CLSID and `os-licensed`).
    pub fn descriptor(&self) -> CodecDescriptor {
        aac_os_descriptor(self.pool.identity().version, true)
    }
}

impl ExternalAudioDecoder for WorkerMfAacProvider {
    fn identity(&self) -> DecoderIdentity {
        self.pool.identity()
    }

    fn supports(&self, codec: AudioCodec) -> bool {
        codec == AudioCodec::AacLc
    }

    /// Admission before any worker starts: AAC-LC, 1-8 channels, a rate of 8 kHz ..= 96 kHz, channel
    /// configuration 1-7 and an AudioSpecificConfig of 2 ..= 64 bytes (reel parsed it already).
    fn open(
        &self,
        spec: &AudioStreamSpec<'_>,
        cancel: &CancellationToken,
    ) -> Result<Box<dyn AudioDecodeSession>, ReelError> {
        if spec.codec != AudioCodec::AacLc {
            return Err(ReelError::CodecUnavailable {
                codec: spec.codec.as_str().to_owned(),
            });
        }
        if !(1..=8).contains(&spec.channels)
            || !(7_350..=96_000).contains(&spec.sample_rate)
            || !(1..=7).contains(&spec.channel_config)
            || !(2..=64).contains(&spec.config.len())
        {
            return Err(ReelError::Unsupported {
                what: "mf-aac-config",
            });
        }
        let mut config = Vec::with_capacity(7_usize.saturating_add(spec.config.len()));
        config.extend_from_slice(&spec.sample_rate.to_le_bytes());
        config.extend_from_slice(&spec.channels.to_le_bytes());
        config.push(spec.channel_config);
        config.extend_from_slice(spec.config);
        let open = OpenRequest {
            max_chunk_bytes: u64::try_from(spec.limits.max_chunk_bytes).unwrap_or(u64::MAX),
            color: ColorTag::Untagged,
            config,
        };
        // Largest reply: one block of MAX_EXTERNAL_BLOCK_FRAMES frames of f32.
        let cap = hsk_studio_reel::audio::MAX_EXTERNAL_BLOCK_FRAMES
            .saturating_mul(usize::from(spec.channels))
            .saturating_mul(4);
        let raw = RawSession::open(
            &self.pool,
            &open,
            cap,
            PayloadKind::Audio(spec.channels, spec.sample_rate),
            cancel,
        )?;
        Ok(Box::new(MfAacSession { raw, next_tag: 0 }))
    }
}

struct MfAacSession {
    raw: RawSession,
    next_tag: u64,
}

fn collect_audio(out: &mut Vec<f32>) -> impl FnMut(Payload) -> Result<(), ReelError> + '_ {
    move |payload| match payload {
        Payload::Audio(block) => {
            out.extend_from_slice(&block.samples);
            Ok(())
        }
        Payload::Picture(_) | Payload::Coded(_) => Err(protocol()),
    }
}

impl AudioDecodeSession for MfAacSession {
    fn decode(
        &mut self,
        access_unit: &[u8],
        cancel: &CancellationToken,
        out: &mut Vec<f32>,
    ) -> Result<usize, ReelError> {
        let tag = self.next_tag;
        self.next_tag = self.next_tag.saturating_add(1);
        let before = out.len();
        let channels = match self.raw.kind {
            PayloadKind::Audio(channels, _) => usize::from(channels),
            _ => 1,
        };
        self.raw.send(tag, access_unit, cancel, &mut collect_audio(out))?;
        Ok(out.len().saturating_sub(before).checked_div(channels).unwrap_or(0))
    }

    fn finish(&mut self, cancel: &CancellationToken, out: &mut Vec<f32>) -> Result<usize, ReelError> {
        let before = out.len();
        let channels = match self.raw.kind {
            PayloadKind::Audio(channels, _) => usize::from(channels),
            _ => 1,
        };
        self.raw.finish(cancel, &mut collect_audio(out))?;
        Ok(out.len().saturating_sub(before).checked_div(channels).unwrap_or(0))
    }
}

#[cfg(feature = "os-encode")]
pub use encode::{MF_ENCODE_MAX_SIDE, WorkerMfEncoder};

#[cfg(feature = "os-encode")]
mod encode {
    use super::*;
    use hsk_studio_reel::{
        EncodedUnit, ExternalVideoEncoder, OS_MAX_BITRATE, OsEncodeSpec, OsVideoCodec,
        VideoEncodeSession,
    };

    /// Largest side an OS-encode session admits (the Microsoft H.264 encoder's 4096 x 2304 is checked by the
    /// encoder itself; this bound only keeps one frame inside a request frame).
    pub const MF_ENCODE_MAX_SIDE: u32 = 8192;

    const H264_SPEC: WorkerSpec = WorkerSpec {
        decoder_id: "mf-h264-encode",
        owner_session: "reel-native-mf-h264-encode",
        io_thread_name: "reel-native-mf-h264-encode-io",
        max_version_bytes: 160,
        codec_label: "h264",
        session_ended: "mf-encode-session-ended",
        packet_bytes: "mf-encode-frame-bytes",
    };

    const HEVC_SPEC: WorkerSpec = WorkerSpec {
        decoder_id: "mf-hevc-encode",
        owner_session: "reel-native-mf-hevc-encode",
        io_thread_name: "reel-native-mf-hevc-encode-io",
        codec_label: "hevc",
        ..H264_SPEC
    };

    /// H.264 / HEVC encode through the OS encoder MFT in isolated workers (feature `os-encode`). Cheap to
    /// clone.
    #[derive(Clone)]
    pub struct WorkerMfEncoder {
        h264: Arc<Pool>,
        hevc: Arc<Pool>,
    }

    impl WorkerMfEncoder {
        pub fn new(options: MfWorkerOptions) -> Result<Self, MfConfigError> {
            let program = check_options(&options)?;
            let pool = |spec: &'static WorkerSpec, arg: &str| {
                Arc::new(Pool::new(
                    spec,
                    program.clone(),
                    vec![arg.to_owned()],
                    Vec::new(),
                    &options.pool_options(),
                ))
            };
            Ok(Self {
                h264: pool(&H264_SPEC, "--encode=h264"),
                hevc: pool(&HEVC_SPEC, "--encode=hevc"),
            })
        }

        fn pool(&self, codec: OsVideoCodec) -> &Arc<Pool> {
            match codec {
                OsVideoCodec::H264 => &self.h264,
                OsVideoCodec::Hevc => &self.hevc,
            }
        }

        pub fn stats(&self, codec: OsVideoCodec) -> MfWorkerStats {
            self.pool(codec).stats()
        }
    }

    impl ExternalVideoEncoder for WorkerMfEncoder {
        /// `mf-h264-encode` / `mf-hevc-encode`; version from the worker HELLO (`<MFT name>|<CLSID>|os-licensed`,
        /// or `unavailable|...` naming why no encoder was found).
        fn identity(&self, codec: OsVideoCodec) -> DecoderIdentity {
            self.pool(codec).identity()
        }

        fn supports(&self, codec: OsVideoCodec) -> bool {
            matches!(codec, OsVideoCodec::H264 | OsVideoCodec::Hevc)
        }

        /// Admission before any worker starts: even sides of 16 ..= `MF_ENCODE_MAX_SIDE`, a frame inside one
        /// request frame, a non-zero frame rate, a bitrate of 1 ..= 400 Mbit/s.
        fn open(
            &self,
            spec: &OsEncodeSpec,
            cancel: &CancellationToken,
        ) -> Result<Box<dyn VideoEncodeSession>, ReelError> {
            let side = |v: u32| (16..=MF_ENCODE_MAX_SIDE).contains(&v) && v.is_multiple_of(2);
            if !side(spec.width) || !side(spec.height) {
                return Err(ReelError::Unsupported {
                    what: "mf-encoder-config",
                });
            }
            if spec.fps_num == 0 || spec.fps_den == 0 || !(1..=OS_MAX_BITRATE).contains(&spec.bitrate) {
                return Err(ReelError::Unsupported {
                    what: "mf-encoder-config",
                });
            }
            let frame_bytes = crate::mf::nv12::i420_bytes(spec.width, spec.height)
                .and_then(|b| usize::try_from(b).ok())
                .filter(|b| *b <= MAX_PACKET_BYTES)
                .ok_or(ReelError::LimitExceeded {
                    what: "mf-encode-frame-bytes",
                })?;
            let mut config = Vec::with_capacity(25);
            config.push(u8::from(spec.codec == OsVideoCodec::Hevc));
            for word in [
                spec.width,
                spec.height,
                spec.fps_num,
                spec.fps_den,
                spec.bitrate,
                spec.profile,
            ] {
                config.extend_from_slice(&word.to_le_bytes());
            }
            let open = OpenRequest {
                max_chunk_bytes: u64::try_from(frame_bytes).unwrap_or(u64::MAX),
                color: ColorTag::Untagged,
                config,
            };
            // A coded unit is bounded by twice the raw frame (far above any real encoder output).
            let raw = RawSession::open(
                self.pool(spec.codec),
                &open,
                frame_bytes.saturating_mul(2),
                PayloadKind::Coded,
                cancel,
            )?;
            Ok(Box::new(MfEncodeSession {
                raw,
                width: spec.width,
                height: spec.height,
                frame: Vec::with_capacity(frame_bytes),
            }))
        }
    }

    struct MfEncodeSession {
        raw: RawSession,
        width: u32,
        height: u32,
        frame: Vec<u8>,
    }

    fn collect_units(out: &mut Vec<EncodedUnit>) -> impl FnMut(Payload) -> Result<(), ReelError> + '_ {
        move |payload| match payload {
            Payload::Coded(unit) => {
                out.push(EncodedUnit {
                    annex_b: unit.bytes,
                    frame_index: unit.tag,
                    keyframe: unit.keyframe,
                });
                Ok(())
            }
            Payload::Picture(_) | Payload::Audio(_) => Err(protocol()),
        }
    }

    impl VideoEncodeSession for MfEncodeSession {
        fn encode(
            &mut self,
            planes: &[&[u8]],
            frame_index: u64,
            cancel: &CancellationToken,
            out: &mut Vec<EncodedUnit>,
        ) -> Result<(), ReelError> {
            let luma = u64::from(self.width).saturating_mul(u64::from(self.height));
            let chroma = luma.checked_div(4).unwrap_or(0);
            let sizes = [luma, chroma, chroma];
            if planes.len() != 3 || planes.iter().zip(sizes).any(|(p, n)| p.len() as u64 != n) {
                return Err(ReelError::Unsupported {
                    what: "mf-encode-frame-bytes",
                });
            }
            self.frame.clear();
            for plane in planes {
                self.frame.extend_from_slice(plane);
            }
            let frame = std::mem::take(&mut self.frame);
            let result = self
                .raw
                .send(frame_index, &frame, cancel, &mut collect_units(out));
            self.frame = frame;
            result
        }

        fn finish(&mut self, cancel: &CancellationToken, out: &mut Vec<EncodedUnit>) -> Result<(), ReelError> {
            self.raw.finish(cancel, &mut collect_units(out))
        }
    }
}
