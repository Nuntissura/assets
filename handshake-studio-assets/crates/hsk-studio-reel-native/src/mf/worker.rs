//! `reel-native-mf-worker` process side (feature `mf-worker`, Windows): hosts the OS-provided Media
//! Foundation H.264 decoder (D24) and serves the same length-prefixed protocol as the AV1 worker
//! (`wire.rs`) on stdin/stdout.
//!
//! Start-up: COM + `MFStartup`, then `MFTEnumEx` lists the H.264 decoder MFTs; the first SYNCHRONOUS,
//! non-hardware one is used (slice 1: software decode, no Direct3D manager). HELLO names decoder id
//! `mf-h264` and a version `<friendly name>|<CLSID>|dxva=0|os-licensed` (spaces become `_`). When Media
//! Foundation cannot start or lists no usable decoder, HELLO still arrives with version
//! `unavailable|<reason>` and every OPEN is answered with the typed `CodecUnavailable h264`.
//!
//! OPEN: the `config` bytes are the host's MF open record: `u32 LE width, u32 LE height` (container-declared,
//! 0 = unknown). PACKET: one Annex B access unit (the host converted it from the ISO BMFF sample and put the
//! SPS/PPS in front of IDR units); its sample time is the packet tag. Pictures are released with the tag
//! equal to their output sample time, which must be the tag of a packet this segment sent and still
//! waits for (`mf-reorder-needs-packet-timing` otherwise; spike S1 decides whether the decoder keeps input
//! times, test `h264_high_bf2_reorder_spike_and_policy`). FINISH drains the decoder.
//!
//! Output: NV12 is copied out of the decoder buffer and cropped/deinterleaved by the pure `nv12` module to
//! tight 8-bit 4:2:0 planes of the container-declared size (else the display aperture, else the frame
//! size). Colour: the container `colr` (origin `Container`) else untagged; the bitstream VUI is not read.
#![deny(clippy::arithmetic_side_effects)]
use super::ffi::{Decoder, DecoderEntry, MfFail, RawPicture, Runtime, enumerate_h264_decoders};
use super::nv12::{Nv12Error, nv12_to_i420};
#[cfg(feature = "test-faults")]
use crate::test_fault::WorkerTestFault;
use crate::wire::{self, OpenRequest, Request, WireFrame};
use hsk_studio_reel::{ChromaFormat, ColorInfo, ColorOrigin, ColorTag, ReelError, VideoFormat};
use std::{
    collections::VecDeque,
    io::{self, Write},
    sync::mpsc,
    thread,
};

/// Decoder id this worker names in HELLO.
pub const MF_DECODER_ID: &str = "mf-h264";
/// Exit code of a worker whose panic hook fired.
pub const MF_PANIC_EXIT_CODE: i32 = 70;

fn unsupported(what: &'static str) -> ReelError {
    ReelError::Unsupported { what }
}

fn unavailable() -> ReelError {
    ReelError::CodecUnavailable {
        codec: "h264".to_owned(),
    }
}

/// HELLO version: printable ASCII only (the host interns `is_ascii_graphic` strings), bounded.
fn version_of(entry: &DecoderEntry) -> String {
    let name: String = entry
        .name
        .chars()
        .map(|c| if c.is_ascii_graphic() { c } else { '_' })
        .take(80)
        .collect();
    format!("{name}|{}|dxva=0|os-licensed", entry.clsid_text())
}

/// The OPEN config record of this worker: declared `(width, height)`.
fn open_size(open: &OpenRequest) -> Result<(u32, u32), ReelError> {
    let bytes: [u8; 8] = open
        .config
        .as_slice()
        .try_into()
        .map_err(|_| ReelError::Corrupt {
            what: "mf-open-config",
        })?;
    let [w0, w1, w2, w3, h0, h1, h2, h3] = bytes;
    Ok((
        u32::from_le_bytes([w0, w1, w2, w3]),
        u32::from_le_bytes([h0, h1, h2, h3]),
    ))
}

fn decode_error(fail: MfFail) -> ReelError {
    match fail {
        MfFail::NoNv12 => unsupported("mf-output-type"),
        MfFail::FormatChange => unsupported("mf-format-change-mid-segment"),
        MfFail::Call(_, _) => ReelError::Corrupt { what: "mf-decode" },
    }
}

struct Segment {
    decoder: Decoder,
    declared: (u32, u32),
    color: ColorTag,
    max_frame_bytes: u64,
    /// Tags of packets sent and not yet released, in send (decode) order.
    waiting: VecDeque<u64>,
    released: bool,
}

impl Segment {
    fn color(&self) -> ColorInfo {
        match self.color {
            ColorTag::Nclx {
                primaries,
                transfer,
                matrix,
                full_range,
            } => ColorInfo {
                primaries,
                transfer,
                matrix,
                full_range: full_range.unwrap_or(false),
                origin: ColorOrigin::Container,
            },
            _ => ColorInfo {
                primaries: 2,
                transfer: 2,
                matrix: 2,
                full_range: false,
                origin: ColorOrigin::Untagged,
            },
        }
    }

    /// Crop size: the container-declared size when it fits the frame, else the display aperture when it
    /// fits, else the frame size.
    fn crop(&self, raw: &RawPicture) -> (u32, u32) {
        let fits = |(w, h): (u32, u32)| w > 0 && h > 0 && w <= raw.width && h <= raw.height;
        let aperture = raw.aperture.and_then(|(w, h)| {
            Some((u32::try_from(w).ok()?, u32::try_from(h).ok()?)).filter(|size| fits(*size))
        });
        if fits(self.declared) {
            self.declared
        } else {
            aperture.unwrap_or((raw.width, raw.height))
        }
    }

    /// Converts one decoder picture into a wire frame; `Err` is the typed refusal for the batch.
    fn frame_of(&mut self, raw: &RawPicture) -> Result<WireFrame, ReelError> {
        let tag = u64::try_from(raw.time).map_err(|_| unsupported("mf-reorder-needs-packet-timing"))?;
        let at = self
            .waiting
            .iter()
            .position(|waiting| *waiting == tag)
            .ok_or_else(|| unsupported("mf-reorder-needs-packet-timing"))?;
        // Packets before it released nothing (configuration-only units, dropped pictures).
        self.waiting.drain(..=at);
        let (width, height) = self.crop(raw);
        // NV12 rows: luma rows = frame height; stride from the buffer size when it divides exactly, else
        // the type's default stride.
        let rows_x2 = u64::from(raw.height).checked_mul(3);
        let len_x2 = (raw.bytes.len() as u64).checked_mul(2);
        let stride = match (rows_x2, len_x2) {
            (Some(rows), Some(len)) if len.checked_rem(rows) == Some(0) => {
                len.checked_div(rows).and_then(|stride| i64::try_from(stride).ok())
            }
            _ => None,
        }
        .or(raw.default_stride.map(i64::from))
        .ok_or(ReelError::Corrupt {
            what: "mf-sample-bytes",
        })?;
        let planes = nv12_to_i420(&raw.bytes, stride, raw.height, width, height, self.max_frame_bytes)
            .map_err(|error| match error {
                Nv12Error::Stride => ReelError::Corrupt {
                    what: "mf-plane-stride",
                },
                Nv12Error::Size => ReelError::Corrupt {
                    what: "mf-picture-size",
                },
                Nv12Error::Bytes => ReelError::Corrupt {
                    what: "mf-sample-bytes",
                },
                Nv12Error::Budget => ReelError::LimitExceeded {
                    what: "mf-frame-bytes",
                },
            })?;
        Ok(WireFrame {
            tag,
            format: VideoFormat {
                width,
                height,
                bit_depth: 8,
                chroma: ChromaFormat::Yuv420,
                color: self.color(),
            },
            film_grain_applied: false,
            planes: planes.into(),
        })
    }

    /// Pulls every picture the decoder releases now and writes it; returns the frames written, or the
    /// typed refusal (the batch then ends with ERROR instead of BATCH_END).
    fn flush(&mut self, out: &mut impl Write, id: u64, count: &mut u32) -> Result<bool, ReelError> {
        let mut pictures = Vec::new();
        let released = self.released;
        self.decoder
            .pull(released, &mut |raw| {
                pictures.push(raw);
                pictures.len() <= wire::MAX_FRAMES_PER_BATCH
            })
            .map_err(decode_error)?;
        for raw in &pictures {
            let frame = self.frame_of(raw)?;
            self.released = true;
            if !reply(out, &wire::encode_frame(id, &frame)) {
                return Ok(false);
            }
            *count = count.saturating_add(1);
        }
        Ok(true)
    }

    /// One PACKET: feed, pulling whenever the decoder refuses input, then release what is ready.
    fn packet(&mut self, out: &mut impl Write, id: u64, tag: u64, bytes: &[u8]) -> Result<(bool, u32), ReelError> {
        let time = i64::try_from(tag).map_err(|_| unsupported("mf-reorder-needs-packet-timing"))?;
        let mut count = 0_u32;
        self.waiting.push_back(tag);
        let mut attempts = 0_u32;
        loop {
            if self.decoder.push(bytes, time).map_err(decode_error)? {
                break;
            }
            // Not accepting: release output first (bounded; a decoder that never accepts is stalled).
            attempts = attempts.saturating_add(1);
            if attempts > 8 {
                return Err(ReelError::Corrupt { what: "mf-decode" });
            }
            if !self.flush(out, id, &mut count)? {
                return Ok((false, count));
            }
        }
        let alive = self.flush(out, id, &mut count)?;
        Ok((alive, count))
    }

    fn finish(&mut self, out: &mut impl Write, id: u64) -> Result<(bool, u32), ReelError> {
        self.decoder.drain().map_err(decode_error)?;
        let mut count = 0_u32;
        let alive = self.flush(out, id, &mut count)?;
        Ok((alive, count))
    }
}

fn reply(out: &mut impl Write, payload: &[u8]) -> bool {
    wire::write_frame(out, payload).is_ok()
}

/// Answer of a batch request: its frames were written; now BATCH_END or the typed ERROR.
fn end_batch(out: &mut impl Write, id: u64, result: Result<(bool, u32), ReelError>) -> (bool, bool) {
    match result {
        Ok((true, count)) => (reply(out, &wire::encode_batch_end(id, count)), true),
        Ok((false, _)) => (false, true),
        Err(error) => (reply(out, &wire::encode_error(id, &error)), false),
    }
}

fn serve(requests: &mpsc::Receiver<Vec<u8>>) -> i32 {
    #[cfg(feature = "test-faults")]
    let fault = WorkerTestFault::from_env();
    let runtime = Runtime::start();
    let chosen = match &runtime {
        Ok(_) => enumerate_h264_decoders()
            .map(|all| {
                all.into_iter()
                    .find(|entry| !entry.hardware && !entry.asynchronous)
            })
            .unwrap_or(None),
        Err(_) => None,
    };
    let version = match (&runtime, &chosen) {
        (Err(code), _) => format!("unavailable|mfstartup-{:08x}", u32::from_ne_bytes(code.to_ne_bytes())),
        (Ok(_), None) => "unavailable|no-sync-software-h264-decoder-mft".to_owned(),
        (Ok(_), Some(entry)) => version_of(entry),
    };
    let stdout = io::stdout();
    let mut out = stdout.lock();
    if !reply(&mut out, &wire::encode_hello(MF_DECODER_ID, &version)) {
        return 1;
    }
    let mut segment: Option<Segment> = None;
    while let Ok(frame) = requests.recv() {
        let Ok(request) = wire::decode_request(&frame) else {
            return 2;
        };
        let alive = match request {
            Request::Open(id, open) => {
                segment = None;
                #[cfg(feature = "test-faults")]
                if fault == WorkerTestFault::ExitOnOpen {
                    std::process::exit(86);
                }
                let opened = match &chosen {
                    None => Err(unavailable()),
                    Some(entry) => open_size(&open).and_then(|declared| {
                        let size = (declared.0 > 0 && declared.1 > 0).then_some(declared);
                        Decoder::open(entry, size)
                            .map_err(decode_error)
                            .map(|decoder| Segment {
                                decoder,
                                declared,
                                color: open.color,
                                max_frame_bytes: open.max_chunk_bytes,
                                waiting: VecDeque::new(),
                                released: false,
                            })
                    }),
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
                    WorkerTestFault::HangOnPacket => {
                        thread::sleep(std::time::Duration::from_secs(30));
                        std::process::exit(3);
                    }
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

/// Entry point of the `reel-native-mf-worker` binary; returns the process exit code. Reads request frames
/// from stdin, writes reply frames to stdout, exits 0 when stdin closes. No window, no console input.
pub fn mf_worker_main() -> i32 {
    std::panic::set_hook(Box::new(|_| std::process::exit(MF_PANIC_EXIT_CODE)));
    let (requests_tx, requests_rx) = mpsc::channel::<Vec<u8>>();
    let reader = thread::Builder::new()
        .name("mf-worker-stdin".to_owned())
        .spawn(move || {
            let stdin = io::stdin();
            let mut input = stdin.lock();
            loop {
                match wire::read_frame(&mut input, wire::MAX_REQUEST_FRAME) {
                    Ok(frame) => {
                        if requests_tx.send(frame).is_err() {
                            return;
                        }
                    }
                    Err(wire::WireError::Closed) => std::process::exit(0),
                    Err(_) => std::process::exit(2),
                }
            }
        });
    if reader.is_err() {
        return 1;
    }
    thread::Builder::new()
        .name("mf-worker-decode".to_owned())
        .spawn(move || serve(&requests_rx))
        .ok()
        .and_then(|handle| handle.join().ok())
        .unwrap_or(1)
}
