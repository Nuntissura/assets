//! `reel-native-mf-worker` modes added by Operator decision D24-A (lane SMB-D24A-CODEC-DEFAULTS), selected
//! by the host with a worker argument and served on the same protocol (`wire.rs`):
//! * `--aac`: the OS AAC decoder MFT (`MFT_CATEGORY_AUDIO_DECODER`, input `{Audio, AAC}`, first synchronous
//!   software MFT). HELLO decoder id `mf-aac`. OPEN config: `u32 rate, u16 channels, u8 channel
//!   configuration, AudioSpecificConfig`. PACKET: one raw access unit; replies AUDIO blocks (interleaved
//!   f32, reordered from the decoder's WAVE channel-mask order to the AAC element order of the channel
//!   configuration, as Studio's decoder delivers) carrying the packet's tag. FINISH drains.
//! * `--encode=h264` / `--encode=hevc` (feature `os-encode`): the OS video encoder MFT (first synchronous
//!   software encoder of that output type). HELLO decoder id `mf-h264-encode` / `mf-hevc-encode`. OPEN
//!   config: `u8 hevc, u32 width, u32 height, u32 fps_num, u32 fps_den, u32 bitrate, u32 profile`. PACKET: tag =
//!   frame index, bytes = tight 8-bit 4:2:0 planes (converted to NV12 here); replies CODED units (Annex B,
//!   clean-point flag) with the tag of the frame they present. FINISH drains.
//!
//! Where Media Foundation or a usable MFT is absent, HELLO still arrives (version `unavailable|<reason>`) and
//! OPEN is answered with the typed `CodecUnavailable` (`aac-lc`, `h264`, `hevc`).
#![deny(clippy::arithmetic_side_effects)]
use super::ffi::{AacDecoder, DecoderEntry, MfFail, PcmOut, Runtime, enumerate_aac_decoders};
#[cfg(feature = "os-encode")]
use super::ffi::{EncodeSetup, Encoder, enumerate_video_encoders};
use super::worker::{end_batch, reply};
#[cfg(feature = "test-faults")]
use crate::test_fault::WorkerTestFault;
use crate::wire::{self, OpenRequest, Request, WireAudio};
#[cfg(feature = "os-encode")]
use crate::wire::WireCoded;
use hsk_studio_reel::ReelError;
#[cfg(feature = "os-encode")]
use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::mpsc;

/// Worker argument of the AAC mode.
pub(crate) const AAC_ARG: &str = "--aac";
/// Worker argument prefix of the encoder modes (`h264` | `hevc`).
pub(crate) const ENCODE_ARG: &str = "--encode=";

/// Decoder id of the AAC mode (reel `AAC_OS_DECODER_ID`).
pub const MF_AAC_DECODER_ID: &str = "mf-aac";
/// Encoder ids of the encode modes.
pub const MF_H264_ENCODER_ID: &str = "mf-h264-encode";
pub const MF_HEVC_ENCODER_ID: &str = "mf-hevc-encode";

/// Most output samples one request may pull before the worker refuses (the host allows 64 replies).
const MAX_PULL: usize = wire::MAX_FRAMES_PER_BATCH;

fn unsupported(what: &'static str) -> ReelError {
    ReelError::Unsupported { what }
}

fn unavailable(codec: &str) -> ReelError {
    ReelError::CodecUnavailable {
        codec: codec.to_owned(),
    }
}

fn version_of(entry: &DecoderEntry) -> String {
    let name: String = entry
        .name
        .chars()
        .map(|c| if c.is_ascii_graphic() { c } else { '_' })
        .take(80)
        .collect();
    format!("{name}|{}|os-licensed", entry.clsid_text())
}

/// The first synchronous software MFT, and the HELLO version (`unavailable|...` naming what was found).
fn choose(
    runtime: &Result<Runtime, i32>,
    list: impl FnOnce() -> Result<Vec<DecoderEntry>, i32>,
    what: &str,
) -> (Option<DecoderEntry>, String) {
    let all = match runtime {
        Err(code) => {
            let code = u32::from_ne_bytes(code.to_ne_bytes());
            return (None, format!("unavailable|mfstartup-{code:08x}"));
        }
        Ok(_) => list().unwrap_or_default(),
    };
    let (hardware, asynchronous) = (
        all.iter().filter(|e| e.hardware).count(),
        all.iter().filter(|e| e.asynchronous && !e.hardware).count(),
    );
    match all.into_iter().find(|e| !e.hardware && !e.asynchronous) {
        Some(entry) => {
            let version = version_of(&entry);
            (Some(entry), version)
        }
        None => (
            None,
            format!("unavailable|no-sync-software-{what}-mft|hw={hardware}|async={asynchronous}"),
        ),
    }
}

/// The AAC element order of `config` as WAVE channel-mask bits; surround pairs accept the back or the
/// side bits. `None`: no reorder (1-2 channels, configuration 7 whose layout decoders read differently).
fn aac_order(config: u8) -> Option<&'static [&'static [u32]]> {
    const C: &[u32] = &[0x4];
    const L: &[u32] = &[0x1];
    const R: &[u32] = &[0x2];
    const CS: &[u32] = &[0x100];
    const LS: &[u32] = &[0x10, 0x200];
    const RS: &[u32] = &[0x20, 0x400];
    const LFE: &[u32] = &[0x8];
    match config {
        3 => Some(&[C, L, R]),
        4 => Some(&[C, L, R, CS]),
        5 => Some(&[C, L, R, LS, RS]),
        6 => Some(&[C, L, R, LS, RS, LFE]),
        _ => None,
    }
}

/// `perm[i]` = index in the decoder's output frame of AAC channel `i`; identity when no reorder applies.
fn permutation(config: u8, out: &PcmOut) -> Result<Vec<usize>, ReelError> {
    let channels = usize::try_from(out.channels).map_err(|_| unsupported("mf-aac-channel-layout"))?;
    let Some(order) = aac_order(config) else {
        return Ok((0..channels).collect());
    };
    if out.mask.count_ones() != out.channels || order.len() != channels {
        return Err(unsupported("mf-aac-channel-layout"));
    }
    // WAVE order: channels appear in increasing mask-bit order.
    let bits: Vec<u32> = (0..32).map(|b| 1_u32 << b).filter(|bit| out.mask & bit != 0).collect();
    order
        .iter()
        .map(|choices| {
            bits.iter()
                .position(|bit| choices.contains(bit))
                .ok_or(unsupported("mf-aac-channel-layout"))
        })
        .collect()
}

struct AacSegment {
    decoder: AacDecoder,
    perm: Vec<usize>,
    channels: u16,
    rate: u32,
    last_tag: Option<u64>,
}

impl AacSegment {
    fn open(entry: &DecoderEntry, open: &OpenRequest) -> Result<Self, ReelError> {
        let bad = || ReelError::Corrupt {
            what: "mf-aac-config",
        };
        let config = open.config.as_slice();
        let rate = u32::from_le_bytes(config.get(0..4).ok_or_else(bad)?.try_into().map_err(|_| bad())?);
        let channels = u16::from_le_bytes(config.get(4..6).ok_or_else(bad)?.try_into().map_err(|_| bad())?);
        let channel_config = *config.get(6).ok_or_else(bad)?;
        let asc = config.get(7..).filter(|a| !a.is_empty()).ok_or_else(bad)?;
        let decoder = AacDecoder::open(entry, rate, u32::from(channels), asc).map_err(|fail| match fail {
            MfFail::NoNv12 => unsupported("mf-aac-output-type"),
            _ => unsupported("mf-aac-config"),
        })?;
        if decoder.out.rate != rate || decoder.out.channels != u32::from(channels) {
            return Err(unsupported("mf-aac-output-type"));
        }
        let perm = permutation(channel_config, &decoder.out)?;
        Ok(Self {
            decoder,
            perm,
            channels,
            rate,
            last_tag: None,
        })
    }

    /// Converts and writes every pulled PCM sample as AUDIO with `tag`.
    fn release(&mut self, out: &mut impl Write, id: u64, tag: u64, count: &mut u32) -> Result<bool, ReelError> {
        let pulled = self
            .decoder
            .pull(MAX_PULL)
            .map_err(|_| ReelError::Corrupt { what: "mf-aac-decode" })?;
        let width = usize::from(self.channels);
        let bytes_per = if self.decoder.out.float { 4 } else { 2 };
        for sample in pulled {
            let frame_bytes = width.saturating_mul(bytes_per);
            if frame_bytes == 0 || sample.bytes.len().checked_rem(frame_bytes) != Some(0) {
                return Err(ReelError::Corrupt {
                    what: "mf-aac-sample-bytes",
                });
            }
            let values: Vec<f32> = if self.decoder.out.float {
                sample
                    .bytes
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    .collect()
            } else {
                sample
                    .bytes
                    .chunks_exact(2)
                    .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0)
                    .collect()
            };
            let mut samples = Vec::with_capacity(values.len());
            for frame in values.chunks_exact(width) {
                samples.extend(self.perm.iter().map(|&i| frame.get(i).copied().unwrap_or(0.0)));
            }
            if samples.is_empty() {
                continue;
            }
            let block = WireAudio {
                tag,
                channels: self.channels,
                rate: self.rate,
                samples,
            };
            if !reply(out, &wire::encode_audio(id, &block)) {
                return Ok(false);
            }
            *count = count.saturating_add(1);
        }
        Ok(true)
    }

    fn packet(&mut self, out: &mut impl Write, id: u64, tag: u64, bytes: &[u8]) -> Result<(bool, u32), ReelError> {
        let time = i64::try_from(tag).map_err(|_| ReelError::Corrupt { what: "mf-aac-decode" })?;
        let mut count = 0_u32;
        self.last_tag = Some(tag);
        for _ in 0..8 {
            let accepted = self
                .decoder
                .push(bytes, time.saturating_mul(1024), 1024)
                .map_err(|_| ReelError::Corrupt { what: "mf-aac-decode" })?;
            if accepted {
                let alive = self.release(out, id, tag, &mut count)?;
                return Ok((alive, count));
            }
            if !self.release(out, id, tag, &mut count)? {
                return Ok((false, count));
            }
        }
        Err(ReelError::Corrupt { what: "mf-aac-decode" })
    }

    fn finish(&mut self, out: &mut impl Write, id: u64) -> Result<(bool, u32), ReelError> {
        self.decoder
            .drain()
            .map_err(|_| ReelError::Corrupt { what: "mf-aac-decode" })?;
        let mut count = 0_u32;
        let Some(tag) = self.last_tag else {
            return Ok((true, 0));
        };
        let alive = self.release(out, id, tag, &mut count)?;
        Ok((alive, count))
    }
}

/// One request loop for the AAC mode.
pub(crate) fn serve_aac(requests: &mpsc::Receiver<Vec<u8>>) -> i32 {
    #[cfg(feature = "test-faults")]
    let fault = WorkerTestFault::from_env();
    let runtime = Runtime::start();
    let (chosen, version) = choose(&runtime, enumerate_aac_decoders, "aac-decoder");
    let stdout = io::stdout();
    let mut out = stdout.lock();
    if !reply(&mut out, &wire::encode_hello(MF_AAC_DECODER_ID, &version)) {
        return 1;
    }
    let mut segment: Option<AacSegment> = None;
    while let Ok(frame) = requests.recv() {
        let Ok(request) = wire::decode_request(&frame) else {
            return 2;
        };
        let alive = match request {
            Request::Open(id, open) => {
                segment = None;
                let opened = match &chosen {
                    None => Err(unavailable("aac-lc")),
                    Some(entry) => AacSegment::open(entry, &open),
                };
                match opened {
                    Ok(opened) => {
                        segment = Some(opened);
                        reply(&mut out, &wire::encode_opened(id))
                    }
                    Err(error) => reply(&mut out, &wire::encode_error(id, &error)),
                }
            }
            Request::Packet(id, tag, bytes) => {
                #[cfg(feature = "test-faults")]
                match fault {
                    WorkerTestFault::ExitOnPacket => std::process::exit(86),
                    WorkerTestFault::GarbageReply => {
                        if !reply(&mut out, &[0xEE, 1, 2, 3]) {
                            return 1;
                        }
                        continue;
                    }
                    _ => {}
                }
                match segment.as_mut() {
                    None => reply(&mut out, &wire::encode_error(id, &unsupported("mf-worker-no-open"))),
                    Some(active) => {
                        let result = active.packet(&mut out, id, tag, &bytes);
                        let (alive, keep) = end_batch(&mut out, id, result);
                        if !keep {
                            segment = None;
                        }
                        alive
                    }
                }
            }
            Request::Finish(id) => match segment.take() {
                None => reply(&mut out, &wire::encode_error(id, &unsupported("mf-worker-no-open"))),
                Some(mut active) => {
                    let result = active.finish(&mut out, id);
                    end_batch(&mut out, id, result).0
                }
            },
        };
        if !alive {
            return 1;
        }
    }
    drop(segment);
    drop(chosen);
    drop(runtime);
    0
}

#[cfg(feature = "os-encode")]
struct EncodeSegment {
    encoder: Encoder,
    width: u32,
    height: u32,
    /// Frame duration in 100 ns units and the input time of every frame still owed (time -> tag).
    duration: i64,
    owed: HashMap<i64, u64>,
}

#[cfg(feature = "os-encode")]
impl EncodeSegment {
    fn open(entry: &DecoderEntry, open: &OpenRequest, hevc: bool) -> Result<Self, ReelError> {
        let bad = || ReelError::Corrupt {
            what: "mf-encoder-config",
        };
        let config = open.config.as_slice();
        let word = |at: usize| -> Result<u32, ReelError> {
            let end = at.checked_add(4).ok_or_else(bad)?;
            Ok(u32::from_le_bytes(config.get(at..end).ok_or_else(bad)?.try_into().map_err(|_| bad())?))
        };
        if config.len() != 25 || config.first() != Some(&u8::from(hevc)) {
            return Err(bad());
        }
        let setup = EncodeSetup {
            hevc,
            width: word(1)?,
            height: word(5)?,
            fps_num: word(9)?,
            fps_den: word(13)?,
            bitrate: word(17)?,
            profile: word(21)?,
        };
        if setup.fps_num == 0 || setup.fps_den == 0 {
            return Err(bad());
        }
        let duration = i64::from(setup.fps_den)
            .checked_mul(10_000_000)
            .and_then(|d| d.checked_div(i64::from(setup.fps_num)))
            .filter(|d| *d > 0)
            .ok_or_else(bad)?;
        let encoder = Encoder::open(entry, &setup).map_err(|_| unsupported("mf-encoder-config"))?;
        Ok(Self {
            encoder,
            width: setup.width,
            height: setup.height,
            duration,
            owed: HashMap::new(),
        })
    }

    /// The frame a coded sample presents: its exact input time; else the owed time nearest to it within one
    /// frame duration (an encoder that rounds or offsets times); else, for an encoder that does not keep
    /// input times, the oldest owed frame (output in input order). The host's mux and decode-back check
    /// the result (presentation times must be a permutation; decoded frames are compared with the source).
    fn owed_tag(&mut self, time: i64) -> Result<u64, ReelError> {
        if let Some(tag) = self.owed.remove(&time) {
            return Ok(tag);
        }
        let near = self
            .owed
            .keys()
            .copied()
            .filter(|t| t.abs_diff(time) < self.duration.unsigned_abs())
            .min_by_key(|t| t.abs_diff(time));
        let key = near
            .or_else(|| self.owed.keys().copied().min())
            .ok_or(ReelError::Corrupt {
                what: "mf-encode-time-unknown",
            })?;
        self.owed.remove(&key).ok_or(ReelError::Corrupt {
            what: "mf-encode-time-unknown",
        })
    }

    fn release(&mut self, out: &mut impl Write, id: u64, count: &mut u32) -> Result<bool, ReelError> {
        let pulled = self
            .encoder
            .pull(MAX_PULL)
            .map_err(|_| ReelError::Corrupt { what: "mf-encode" })?;
        for sample in pulled {
            let tag = self.owed_tag(sample.time)?;
            if sample.bytes.is_empty() {
                return Err(ReelError::Corrupt {
                    what: "mf-encoder-output",
                });
            }
            let unit = WireCoded {
                tag,
                keyframe: sample.clean_point,
                bytes: sample.bytes,
            };
            if !reply(out, &wire::encode_coded(id, &unit)) {
                return Ok(false);
            }
            *count = count.saturating_add(1);
        }
        Ok(true)
    }

    fn packet(&mut self, out: &mut impl Write, id: u64, tag: u64, planes: &[u8]) -> Result<(bool, u32), ReelError> {
        let nv12 = super::nv12::i420_to_nv12(planes, self.width, self.height).ok_or(ReelError::Corrupt {
            what: "mf-encode-frame-bytes",
        })?;
        let time = i64::try_from(tag)
            .ok()
            .and_then(|t| t.checked_mul(self.duration))
            .ok_or(ReelError::Corrupt { what: "mf-encode" })?;
        self.owed.insert(time, tag);
        let mut count = 0_u32;
        for _ in 0..8 {
            let accepted = self
                .encoder
                .push(&nv12, time, self.duration)
                .map_err(|_| ReelError::Corrupt { what: "mf-encode" })?;
            if accepted {
                let alive = self.release(out, id, &mut count)?;
                return Ok((alive, count));
            }
            if !self.release(out, id, &mut count)? {
                return Ok((false, count));
            }
        }
        Err(ReelError::Corrupt { what: "mf-encode" })
    }

    fn finish(&mut self, out: &mut impl Write, id: u64) -> Result<(bool, u32), ReelError> {
        self.encoder
            .drain()
            .map_err(|_| ReelError::Corrupt { what: "mf-encode" })?;
        let mut count = 0_u32;
        let alive = self.release(out, id, &mut count)?;
        Ok((alive, count))
    }
}

/// One request loop for an encode mode (`hevc` false: H.264).
#[cfg(feature = "os-encode")]
pub(crate) fn serve_encode(requests: &mpsc::Receiver<Vec<u8>>, hevc: bool) -> i32 {
    use windows::Win32::Media::MediaFoundation::{MFVideoFormat_H264, MFVideoFormat_HEVC};
    let runtime = Runtime::start();
    let (subtype, codec, id_name, what) = if hevc {
        (MFVideoFormat_HEVC, "hevc", MF_HEVC_ENCODER_ID, "hevc-encoder")
    } else {
        (MFVideoFormat_H264, "h264", MF_H264_ENCODER_ID, "h264-encoder")
    };
    let (chosen, version) = choose(&runtime, || enumerate_video_encoders(&subtype), what);
    let stdout = io::stdout();
    let mut out = stdout.lock();
    if !reply(&mut out, &wire::encode_hello(id_name, &version)) {
        return 1;
    }
    let mut segment: Option<EncodeSegment> = None;
    while let Ok(frame) = requests.recv() {
        let Ok(request) = wire::decode_request(&frame) else {
            return 2;
        };
        let alive = match request {
            Request::Open(id, open) => {
                segment = None;
                let opened = match &chosen {
                    None => Err(unavailable(codec)),
                    Some(entry) => EncodeSegment::open(entry, &open, hevc),
                };
                match opened {
                    Ok(opened) => {
                        segment = Some(opened);
                        reply(&mut out, &wire::encode_opened(id))
                    }
                    Err(error) => reply(&mut out, &wire::encode_error(id, &error)),
                }
            }
            Request::Packet(id, tag, bytes) => match segment.as_mut() {
                None => reply(&mut out, &wire::encode_error(id, &unsupported("mf-worker-no-open"))),
                Some(active) => {
                    let result = active.packet(&mut out, id, tag, &bytes);
                    let (alive, keep) = end_batch(&mut out, id, result);
                    if !keep {
                        segment = None;
                    }
                    alive
                }
            },
            Request::Finish(id) => match segment.take() {
                None => reply(&mut out, &wire::encode_error(id, &unsupported("mf-worker-no-open"))),
                Some(mut active) => {
                    let result = active.finish(&mut out, id);
                    end_batch(&mut out, id, result).0
                }
            },
        };
        if !alive {
            return 1;
        }
    }
    drop(segment);
    drop(chosen);
    drop(runtime);
    0
}
