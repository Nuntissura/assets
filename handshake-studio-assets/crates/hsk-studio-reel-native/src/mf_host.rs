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
//! Microsoft decoder needs Annex B and skips input until it has an SPS and a PPS). The packet tag is the
//! input sample time; the decoder keeps it on the picture and releases pictures in PRESENTATION order
//! (spike S1 strategy A), so the session declares reel's `reorders_output`: reel matches each picture to
//! its own packet and B-frame streams decode exactly (test `h264_bframe_streams_decode_exact_in_presentation_order`).
//!
//! Paths (`MfPath`): `Software` (default; the decoder MFT without a Direct3D manager), `Hardware` (DXVA: the
//! worker creates a headless Direct3D 11 video device and hands it to the decoder through a DXGI device
//! manager; a host or stream the decoder cannot accelerate is `Unsupported mf-dxva-unavailable`, never a
//! silent switch) and `Auto` (hardware first, then the software pool when the hardware pool answers
//! `mf-dxva-unavailable` or `MEDIA_CODEC_UNAVAILABLE`). The receipt's decoder version says which path
//! decoded the segment (`dxva=1` or `dxva=0`). DXVA output is read back through the decoder's own buffer
//! lock; its equality with the software output is proven per host by test `mf_dxva_parity_with_software`
//! (NOT_PROVEN on a host without a Direct3D 11 video device). Exports should keep `Software` until a parity
//! receipt exists for the device (lane brief R3).
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

pub(crate) const MF_SPEC: WorkerSpec = WorkerSpec {
    decoder_id: MF_H264_PROVIDER_ID,
    owner_session: "reel-native-mf-h264",
    io_thread_name: "reel-native-mf-worker-io",
    max_version_bytes: 160,
    codec_label: "h264",
    session_ended: "mf-worker-session-ended",
    packet_bytes: "h264-packet-bytes",
};

/// The DXVA pool: same decoder id, own process owner tag.
pub(crate) const MF_DXVA_SPEC: WorkerSpec = WorkerSpec {
    owner_session: "reel-native-mf-h264-dxva",
    io_thread_name: "reel-native-mf-dxva-worker-io",
    ..MF_SPEC
};

/// Worker argument of the first-worker exit fault (test-faults; the worker's `FIRST_WORKER_EXIT_ARG`).
#[cfg(feature = "test-faults")]
const FIRST_WORKER_EXIT_ARG: &str = "--test-exit-at-tag=";

/// Worker argument of the DXVA pool (the worker's `DXVA_ARG`).
const DXVA_ARG: &str = "--dxva";
/// Refusal of a hardware pool that cannot accelerate (no device, or the decoder refused the manager).
pub const MF_DXVA_UNAVAILABLE: &str = "mf-dxva-unavailable";

/// Which decode path a provider uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MfPath {
    /// The decoder MFT without a Direct3D manager (exact; the default).
    #[default]
    Software,
    /// DXVA through a headless Direct3D 11 video device; `Unsupported mf-dxva-unavailable` where it cannot.
    Hardware,
    /// Hardware first, software when the hardware pool is unavailable for the stream.
    Auto,
}

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
    /// Decode path (default `Software`).
    pub path: MfPath,
    /// A worker whose HEARTBEAT counter (advanced by its decode loop, never by a timer) does not advance for
    /// this long is killed (`decoder_deadline`, counted in `killed_heartbeat`). Default 2 s.
    pub heartbeat_timeout: Duration,
    /// Bytes of prepared packets retained since the last IDR packet for one replay on a fresh worker after a
    /// crash or deadline (0 = no replay). Default 64 MiB.
    pub replay_bytes: usize,
    /// Fault only the FIRST worker of a pool injects: it exits when it receives the packet with this tag
    /// (`test-faults` only; replay tests).
    #[cfg(feature = "test-faults")]
    pub first_worker_exit_at_tag: Option<u64>,
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
            path: MfPath::Software,
            heartbeat_timeout: Duration::from_secs(2),
            replay_bytes: 64 << 20,
            #[cfg(feature = "test-faults")]
            first_worker_exit_at_tag: None,
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

    /// The MF-only pool settings: heartbeat watch and the first-worker test fault.
    fn finish_pool(&self, pool: Pool) -> Pool {
        let pool = pool.with_heartbeat(self.heartbeat_timeout);
        #[cfg(feature = "test-faults")]
        let pool = pool.with_first_spawn_args(
            self.first_worker_exit_at_tag
                .map(|tag| vec![format!("{FIRST_WORKER_EXIT_ARG}{tag}")])
                .unwrap_or_default(),
        );
        pool
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
    /// Software pool (`Software`, `Auto`) and DXVA pool (`Hardware`, `Auto`); at least one exists.
    software: Option<Arc<Pool>>,
    hardware: Option<Arc<Pool>>,
    /// Replay retention cap per segment (0 = no replay).
    replay_bytes: usize,
}

thread_local! {
    /// Which pool served the last successful `open` on this thread: reel's route reads `identity()` right
    /// after `open` on the same thread, so the receipt names the path that decodes the segment.
    static LAST_HARDWARE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The one variable the DXVA worker gets on top of the empty environment: `SystemRoot` (a system path, no
/// secret). Measured 2026-10-14 on this host: `D3D11CreateDevice(..., VIDEO_SUPPORT)` in a worker with an
/// empty environment fails with `DXGI_ERROR_UNSUPPORTED` (0x887A0004) and succeeds with `SystemRoot` alone.
fn dxva_env() -> Vec<(String, String)> {
    std::env::var("SystemRoot")
        .map(|value| vec![("SystemRoot".to_owned(), value)])
        .unwrap_or_default()
}

/// Hardware-pool answers after which `Auto` uses the software pool.
fn falls_back(error: &VideoDecodeError) -> bool {
    matches!(
        error,
        VideoDecodeError::Media(ReelError::CodecUnavailable { .. })
            | VideoDecodeError::Media(ReelError::Unsupported {
                what: MF_DXVA_UNAVAILABLE
            })
    )
}

impl WorkerMfProvider {
    pub fn new(options: MfWorkerOptions) -> Result<Self, MfConfigError> {
        if options.max_workers == 0
            || options.max_spawns == 0
            || options.spawn_window.is_zero()
            || options.spawn_timeout.is_zero()
            || options.batch_deadline.is_zero()
            || options.segment_deadline.is_zero()
            || options.heartbeat_timeout.is_zero()
        {
            return Err(Av1ConfigError::PolicyInvalid);
        }
        let program = resolve_program(options.program.clone(), MF_WORKER_PROGRAM)
            .ok_or(Av1ConfigError::WorkerProgramMissing)?;
        let pool_options = options.pool_options();
        let software = matches!(options.path, MfPath::Software | MfPath::Auto)
            .then(|| {
                Arc::new(options.finish_pool(Pool::new(
                    &MF_SPEC,
                    program.clone(),
                    Vec::new(),
                    Vec::new(),
                    &pool_options,
                )))
            });
        let hardware = matches!(options.path, MfPath::Hardware | MfPath::Auto).then(|| {
            Arc::new(options.finish_pool(Pool::new(
                &MF_DXVA_SPEC,
                program,
                vec![DXVA_ARG.to_owned()],
                dxva_env(),
                &pool_options,
            )))
        });
        Ok(Self {
            software,
            hardware,
            replay_bytes: options.replay_bytes,
        })
    }

    /// The pool `identity()` and `descriptor()` describe: the one that served this thread's last open,
    /// else the software pool when there is one.
    fn pool(&self) -> &Arc<Pool> {
        let hardware_last = LAST_HARDWARE.with(std::cell::Cell::get);
        match (&self.software, &self.hardware) {
            (Some(_), Some(hardware)) if hardware_last => hardware,
            (Some(software), _) => software,
            (None, Some(hardware)) => hardware,
            (None, None) => unreachable!("WorkerMfProvider::new builds at least one pool"),
        }
    }

    /// Counters of the software pool (or of the DXVA pool of a `Hardware` provider).
    pub fn stats(&self) -> MfWorkerStats {
        self.software
            .as_ref()
            .or(self.hardware.as_ref())
            .map(|pool| pool.stats())
            .unwrap_or_default()
    }

    /// Counters of the DXVA pool (`None` for a `Software` provider).
    pub fn hardware_stats(&self) -> Option<MfWorkerStats> {
        self.hardware.as_ref().map(|pool| pool.stats())
    }

    /// Capability descriptor: decoder `mf-h264`, version learned from the worker HELLO (`unknown` before the
    /// first worker started; it names the MFT, its CLSID, `dxva=0` and `os-licensed`), licence
    /// "Windows Media Foundation H.264 decoder, OS-licensed", decode only.
    pub fn descriptor(&self) -> CodecDescriptor {
        h264_descriptor(self.pool().identity().version, true)
    }

    fn open_session(
        &self,
        spec: &VideoStreamSpec<'_>,
        cancel: &CancellationToken,
    ) -> Result<WorkerSession, VideoDecodeError> {
        if let Some(hardware) = &self.hardware {
            match self.open_on(hardware, spec, cancel) {
                Ok(session) => {
                    LAST_HARDWARE.with(|last| last.set(true));
                    return Ok(session);
                }
                Err(error) if self.software.is_some() && falls_back(&error) => {}
                Err(error) => return Err(error),
            }
        }
        let software = self.software.as_ref().ok_or(ReelError::Unsupported {
            what: MF_DXVA_UNAVAILABLE,
        })?;
        let session = self.open_on(software, spec, cancel)?;
        LAST_HARDWARE.with(|last| last.set(false));
        Ok(session)
    }

    fn open_on(
        &self,
        pool: &Arc<Pool>,
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
            pool,
            OpenPlan {
                open,
                frame_cap,
                declared: (spec.width, spec.height),
                max_packet_bytes: spec.limits.max_chunk_bytes,
                prep: Some(annex_b_prep(config)),
                reorders_output: true,
                replay_cap: (self.replay_bytes > 0).then_some(self.replay_bytes),
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
        )?;
        // A packet carrying the parameter sets (segment start or IDR) is where a fresh decoder can start.
        Ok(with_sets)
    })
}

impl ExternalVideoDecoder for WorkerMfProvider {
    /// `mf-h264`; version learned from the first worker HELLO (`unknown` before).
    fn identity(&self) -> DecoderIdentity {
        self.pool().identity()
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
