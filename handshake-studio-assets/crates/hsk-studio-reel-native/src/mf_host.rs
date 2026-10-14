//! Host side of the H.264 provider (feature `mf`, pure Rust, no `unsafe`): `WorkerMfProvider` is reel's
//! `ExternalVideoDecoder` for `VideoCodec::H264` and drives `reel-native-mf-worker` processes, which host the
//! OS-provided Windows Media Foundation H.264 decoder (D24: H.264 decode only through that decoder, in an
//! isolated worker; typed `MEDIA_CODEC_UNAVAILABLE` where it is absent; no encode).
//!
//! Register it with `Registry::with_video_decoder(Arc::new(provider))`: reel routes UNTRUSTED (and trusted:
//! there is no in-process H.264 decoder) `avc1`/`avc3` ISO BMFF tracks to it. The pool, supervision (cancel,
//! deadlines, output cap, protocol checks, bounded respawn, empty environment, `CREATE_NO_WINDOW`) and the
//! wire are the AV1 worker's (`host.rs`, `wire.rs`, process control in `hsk-studio-worker`): one core.
//!
//! Before any worker starts (from the container-declared stream, hostile input, checked arithmetic): an
//! `avcC` that does not parse, a profile other than Baseline/Main/High (profile_idc 66/77/100), chroma other
//! than 4:2:0, bit depth other than 8 -> typed `Unsupported`; declared dimensions whose 4:2:0 byte size
//! overflows -> `LimitExceeded h264-declared-dimensions`, above `Limits::max_chunk_bytes` ->
//! `LimitExceeded h264-frame-bytes`, outside the Microsoft decoder's 48..=4096 x 48..=2304 (Learn "H.264
//! video decoder") -> `Unsupported mf-picture-size` (test `mf_declared_limits_and_unsupported_streams`).
//!
//! Packets: the host converts every ISO BMFF sample (length-prefixed NAL units) into Annex B and puts the
//! `avcC` SPS/PPS in front of the first packet of a segment and of every packet holding an IDR slice (the
//! Microsoft decoder needs Annex B and skips input until it has an SPS and a PPS). Pictures must leave the
//! decoder in packet order: a stream whose decoder releases pictures in another order (B-frame reordering)
//! is refused with `Unsupported mf-reorder-needs-packet-timing` (reel's pump matches pictures to packets in
//! decode order; presentation-order delivery needs packet timing, reel prerequisite P2).
#![deny(clippy::arithmetic_side_effects)]
use crate::host::{
    Av1ConfigError, Av1WorkerOptions, Av1WorkerStats, MAX_PACKET_BYTES, OpenPlan,
    PacketPrep, Pool, WorkerSession, WorkerSpec, open_worker,
};
use crate::mf::nv12::i420_bytes;
use crate::wire::OpenRequest;
#[cfg(feature = "test-faults")]
use crate::test_fault::WorkerTestFault;
use hsk_studio_accord::CancellationToken;
use hsk_studio_reel::{
    CodecDescriptor, DecoderIdentity, ExternalVideoDecoder, H264Config, ReelError, VideoCodec,
    VideoDecodeError, VideoDecodeSession, VideoStreamSpec, h264_descriptor, h264_nal_unit_type,
    parse_avcc, sample_to_annex_b,
};
use hsk_studio_worker::resolve_program;
use std::{path::PathBuf, sync::Arc, time::Duration};

/// Decoder id of this provider (receipts, HELLO).
pub const MF_H264_PROVIDER_ID: &str = "mf-h264";
/// File name of the worker executable (without the platform suffix).
pub const MF_WORKER_PROGRAM: &str = "reel-native-mf-worker";
/// Microsoft H.264 decoder limits (Learn "H.264 video decoder": minimum 48 x 48, maximum 4096 x 2304).
pub const MF_MIN_SIDE: u32 = 48;
pub const MF_MAX_WIDTH: u32 = 4096;
pub const MF_MAX_HEIGHT: u32 = 2304;
/// Reason a reordering stream is refused until reel gives sessions packet timing (P2).
pub const MF_REORDER_REFUSAL: &str = "mf-reorder-needs-packet-timing";

pub(crate) const MF_SPEC: WorkerSpec = WorkerSpec {
    decoder_id: MF_H264_PROVIDER_ID,
    owner_session: "reel-native-mf-h264",
    io_thread_name: "reel-native-mf-worker-io",
    max_version_bytes: 160,
    codec_label: "h264",
    session_ended: "mf-worker-session-ended",
    packet_bytes: "h264-packet-bytes",
};

/// Pool policy of the H.264 worker pool (same meaning as `Av1WorkerOptions`).
#[derive(Clone, Debug)]
pub struct MfWorkerOptions {
    /// Worker executable. `None`: `reel-native-mf-worker` in the directory of the running executable only.
    pub program: Option<PathBuf>,
    pub max_workers: usize,
    pub max_spawns: u32,
    pub spawn_window: Duration,
    pub spawn_timeout: Duration,
    pub batch_deadline: Duration,
    pub segment_deadline: Duration,
    /// Fault the spawned workers inject (`test-faults` feature only).
    #[cfg(feature = "test-faults")]
    pub test_fault: WorkerTestFault,
}

impl Default for MfWorkerOptions {
    fn default() -> Self {
        let base = Av1WorkerOptions::default();
        Self {
            program: None,
            max_workers: base.max_workers,
            max_spawns: base.max_spawns,
            spawn_window: base.spawn_window,
            spawn_timeout: base.spawn_timeout,
            batch_deadline: base.batch_deadline,
            segment_deadline: base.segment_deadline,
            #[cfg(feature = "test-faults")]
            test_fault: WorkerTestFault::None,
        }
    }
}

impl MfWorkerOptions {
    pub fn with_worker_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = Some(program.into());
        self
    }

    fn pool_options(&self) -> Av1WorkerOptions {
        Av1WorkerOptions {
            program: self.program.clone(),
            max_workers: self.max_workers,
            max_spawns: self.max_spawns,
            spawn_window: self.spawn_window,
            spawn_timeout: self.spawn_timeout,
            batch_deadline: self.batch_deadline,
            segment_deadline: self.segment_deadline,
            #[cfg(feature = "test-faults")]
            test_fault: self.test_fault,
        }
    }
}

/// Process counters of the H.264 worker pool (same fields as the AV1 pool's).
pub type MfWorkerStats = Av1WorkerStats;
/// Why a provider could not be built (same values as the AV1 provider's).
pub type MfConfigError = Av1ConfigError;

/// Process-isolated H.264 decoder through Windows Media Foundation (host side). Cheap to clone.
#[derive(Clone)]
pub struct WorkerMfProvider {
    pool: Arc<Pool>,
}

impl WorkerMfProvider {
    pub fn new(options: MfWorkerOptions) -> Result<Self, MfConfigError> {
        if options.max_workers == 0
            || options.max_spawns == 0
            || options.spawn_window.is_zero()
            || options.spawn_timeout.is_zero()
            || options.batch_deadline.is_zero()
            || options.segment_deadline.is_zero()
        {
            return Err(Av1ConfigError::PolicyInvalid);
        }
        let program = resolve_program(options.program.clone(), MF_WORKER_PROGRAM)
            .ok_or(Av1ConfigError::WorkerProgramMissing)?;
        Ok(Self {
            pool: Arc::new(Pool::new(&MF_SPEC, program, &options.pool_options())),
        })
    }

    pub fn stats(&self) -> MfWorkerStats {
        self.pool.stats()
    }

    /// Capability descriptor: decoder `mf-h264`, version learned from the worker HELLO (`unknown` before the
    /// first worker started; it names the MFT, its CLSID, `dxva=0` and `os-licensed`), licence
    /// "Windows Media Foundation H.264 decoder, OS-licensed", decode only.
    pub fn descriptor(&self) -> CodecDescriptor {
        h264_descriptor(self.pool.identity().version, true)
    }

    fn open_session(
        &self,
        spec: &VideoStreamSpec<'_>,
        cancel: &CancellationToken,
    ) -> Result<WorkerSession, VideoDecodeError> {
        cancel.check().map_err(|_| ReelError::Canceled)?;
        if spec.codec != VideoCodec::H264 {
            return Err(ReelError::CodecUnavailable {
                codec: spec.codec.as_str().to_owned(),
            }
            .into());
        }
        let (config, frame_cap) = admit_stream(spec)?;
        let mut open_config = Vec::with_capacity(8);
        open_config.extend_from_slice(&spec.width.to_le_bytes());
        open_config.extend_from_slice(&spec.height.to_le_bytes());
        let open = OpenRequest {
            max_chunk_bytes: u64::try_from(frame_cap).unwrap_or(u64::MAX),
            color: spec.color,
            config: open_config,
        };
        open_worker(
            &self.pool,
            OpenPlan {
                open,
                frame_cap,
                declared: (spec.width, spec.height),
                max_packet_bytes: spec.limits.max_chunk_bytes,
                prep: Some(annex_b_prep(config)),
                decode_order_only: Some(MF_REORDER_REFUSAL),
            },
            None,
            cancel,
        )
    }
}

/// The pre-spawn admission of one declared stream: the parsed record and the largest packed picture it
/// allows (reply cap basis). Every refusal here happens before any worker starts.
pub(crate) fn admit_stream(spec: &VideoStreamSpec<'_>) -> Result<(H264Config, usize), ReelError> {
    if spec.config.is_empty() {
        return Err(ReelError::Corrupt {
            what: "avcc-missing",
        });
    }
    let config = parse_avcc(spec.config)?;
    if !matches!(config.profile_idc, 66 | 77 | 100) {
        return Err(ReelError::Unsupported {
            what: "h264-profile",
        });
    }
    if config.chroma_format_or_420() != 1 {
        return Err(ReelError::Unsupported {
            what: "h264-chroma-format",
        });
    }
    if config.bit_depth_luma_or_8() != 8 || config.bit_depth_chroma.is_some_and(|depth| depth != 8) {
        return Err(ReelError::Unsupported {
            what: "h264-bit-depth",
        });
    }
    let (width, height) = (spec.width, spec.height);
    let max = spec.limits.max_chunk_bytes;
    if width == 0 || height == 0 {
        return Ok((config, max));
    }
    let bytes = i420_bytes(width, height).ok_or(ReelError::LimitExceeded {
        what: "h264-declared-dimensions",
    })?;
    let bytes = usize::try_from(bytes)
        .ok()
        .filter(|bytes| *bytes <= max)
        .ok_or(ReelError::LimitExceeded {
            what: "h264-frame-bytes",
        })?;
    if !(MF_MIN_SIDE..=MF_MAX_WIDTH).contains(&width)
        || !(MF_MIN_SIDE..=MF_MAX_HEIGHT).contains(&height)
    {
        return Err(ReelError::Unsupported {
            what: "mf-picture-size",
        });
    }
    Ok((config, bytes))
}

/// True when a length-prefixed sample holds an IDR slice (NAL type 5). A malformed sample answers false;
/// the conversion that follows reports it.
fn has_idr(sample: &[u8], nal_length_size: u8) -> bool {
    let size = usize::from(nal_length_size);
    let mut rest = sample;
    while let Some((field, tail)) = rest.split_at_checked(size) {
        let len = field
            .iter()
            .fold(0u32, |acc, byte| acc.wrapping_shl(8) | u32::from(*byte));
        let Ok(len) = usize::try_from(len) else {
            return false;
        };
        let Some((nal, after)) = tail.split_at_checked(len) else {
            return false;
        };
        if nal.first().is_some_and(|header| h264_nal_unit_type(*header) == 5) {
            return true;
        }
        if after.len() == rest.len() {
            return false;
        }
        rest = after;
    }
    false
}

/// ISO BMFF sample -> Annex B, parameter sets in front of the first packet and of every IDR packet.
fn annex_b_prep(config: H264Config) -> PacketPrep {
    let mut first = true;
    Box::new(move |packet, out| {
        let with_sets = first || has_idr(packet, config.nal_length_size);
        first = false;
        sample_to_annex_b(
            packet,
            config.nal_length_size,
            with_sets.then_some(&config),
            MAX_PACKET_BYTES,
            out,
        )
    })
}

impl ExternalVideoDecoder for WorkerMfProvider {
    /// `mf-h264`; version learned from the first worker HELLO (`unknown` before).
    fn identity(&self) -> DecoderIdentity {
        self.pool.identity()
    }

    fn supports(&self, codec: VideoCodec) -> bool {
        codec == VideoCodec::H264
    }

    fn open(
        &self,
        spec: &VideoStreamSpec<'_>,
        cancel: &CancellationToken,
    ) -> Result<Box<dyn VideoDecodeSession>, VideoDecodeError> {
        Ok(Box::new(self.open_session(spec, cancel)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idr_detection_walks_every_nal() {
        // SEI (6) then IDR (5), 4-byte lengths.
        assert!(has_idr(&[0, 0, 0, 1, 0x06, 0, 0, 0, 2, 0x65, 9], 4));
        // Non-IDR slice (1) only.
        assert!(!has_idr(&[0, 0, 0, 2, 0x41, 9], 4));
        // Malformed: zero length loops are cut, a length past the end answers false.
        assert!(!has_idr(&[0, 0, 0, 0, 0, 0, 0, 0], 4));
        assert!(!has_idr(&[0xFF, 0xFF, 0xFF, 0xFF, 0x65], 4));
        assert!(!has_idr(&[], 4));
    }
}
