//! YUV4MPEG2 (Y4M) provider: license-free raw video, 8-bit 4:2:0 / 4:2:2 / 4:4:4 / mono, progressive,
//! plain `FRAME\n` records. Decode streams ONE frame at a time into a reused buffer; peak memory is one
//! frame plus the header. Frame rate must map to an exact broadcast tick duration (`FrameRate`).
//!
//! Every size, offset, count and duration derived from header numbers or caller input uses checked
//! arithmetic and a typed error (V4-01); `v4_hardening.rs` proves the u64/u32-max and aggregate cases.
#![deny(clippy::arithmetic_side_effects)]
use crate::error::{ReelError, check_cancel};
use crate::model::*;
use hsk_studio_accord::{CancellationToken, FrameRate, Ticks};
use std::io::{SeekFrom, Write};

pub const Y4M_PROVIDER_ID: &str = "y4m";
const DECODER_VERSION: &str = "1";
const MAX_HEADER_BYTES: usize = 4096;
const MAX_DIMENSION: u32 = 65_536;
const FRAME_MARKER: &[u8; 6] = b"FRAME\n";
const FRAME_MARKER_LEN: u64 = FRAME_MARKER.len() as u64;

#[derive(Debug, Clone, Copy)]
pub struct Y4mProvider;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Chroma {
    C420,
    C422,
    C444,
    Mono,
}

impl Chroma {
    fn name(self) -> &'static str {
        match self {
            Self::C420 => "420",
            Self::C422 => "422",
            Self::C444 => "444",
            Self::Mono => "mono",
        }
    }
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "420" => Some(Self::C420),
            "422" => Some(Self::C422),
            "444" => Some(Self::C444),
            "mono" => Some(Self::Mono),
            _ => None,
        }
    }
    fn frame_bytes(self, w: u32, h: u32) -> Option<u64> {
        let (w, h) = (u64::from(w), u64::from(h));
        let luma = w.checked_mul(h)?;
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        match self {
            Self::C420 => luma.checked_add(cw.checked_mul(ch)?.checked_mul(2)?),
            Self::C422 => luma.checked_add(cw.checked_mul(h)?.checked_mul(2)?),
            Self::C444 => luma.checked_mul(3),
            Self::Mono => Some(luma),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Y4mInfo {
    width: u32,
    height: u32,
    ticks_per_frame: u64,
    frame_bytes: u64,
    header_bytes: u64,
    /// The 6-byte frame marker plus one frame of pixel bytes.
    record: u64,
    /// Whole `FRAME` records present in the file.
    complete_frames: u64,
}

fn parse_ratio(text: &str) -> Option<(u64, u64)> {
    let (n, d) = text.split_once(':')?;
    let (n, d) = (n.parse::<u64>().ok()?, d.parse::<u64>().ok()?);
    (n > 0 && d > 0).then_some((n, d))
}

fn parse_y4m(src: &mut dyn ReadSeek, file_len: u64, limits: &Limits) -> Result<Y4mInfo, ReelError> {
    let cap = (MAX_HEADER_BYTES as u64)
        .min(limits.max_probe_bytes)
        .min(file_len) as usize;
    let mut buf = vec![0u8; cap];
    src.seek(SeekFrom::Start(0))?;
    let got = {
        let mut filled = 0;
        while filled < cap {
            let n = std::io::Read::read(src, &mut buf[filled..])?;
            if n == 0 {
                break;
            }
            // A `Read` that reports more bytes than the slice it was given is broken: refuse.
            filled = filled
                .checked_add(n)
                .filter(|total| *total <= cap)
                .ok_or(ReelError::Io(std::io::ErrorKind::InvalidData))?;
        }
        filled
    };
    let buf = &buf[..got];
    let Some(end) = buf.iter().position(|b| *b == b'\n') else {
        return Err(if got >= MAX_HEADER_BYTES {
            ReelError::LimitExceeded {
                what: "y4m-header-bytes",
            }
        } else {
            ReelError::Corrupt {
                what: "y4m-header-unterminated",
            }
        });
    };
    let header = std::str::from_utf8(&buf[..end]).map_err(|_| ReelError::Corrupt {
        what: "y4m-header-text",
    })?;
    let mut tokens = header.split(' ');
    if tokens.next() != Some("YUV4MPEG2") {
        return Err(ReelError::ImportUnsupported {
            detected: "not-y4m".into(),
        });
    }
    let (mut width, mut height, mut rate) = (0u32, 0u32, None);
    let mut chroma = Chroma::C420;
    for token in tokens.filter(|t| !t.is_empty()) {
        // The tag is one ASCII byte, so splitting after it is always on a char boundary.
        if !token.as_bytes()[0].is_ascii() {
            return Err(ReelError::Corrupt {
                what: "y4m-header-tag",
            });
        }
        let (tag, value) = token.split_at(1);
        match tag {
            "W" => {
                width = value
                    .parse()
                    .map_err(|_| ReelError::Corrupt { what: "y4m-width" })?
            }
            "H" => {
                height = value
                    .parse()
                    .map_err(|_| ReelError::Corrupt { what: "y4m-height" })?
            }
            "F" => rate = Some(parse_ratio(value).ok_or(ReelError::Corrupt { what: "y4m-rate" })?),
            "I" => {
                if value != "p" && value != "?" {
                    return Err(ReelError::Unsupported {
                        what: "y4m-interlaced",
                    });
                }
            }
            "C" => {
                chroma = match value {
                    "420" | "420jpeg" | "420mpeg2" | "420paldv" => Chroma::C420,
                    "422" => Chroma::C422,
                    "444" => Chroma::C444,
                    "mono" => Chroma::Mono,
                    _ => {
                        return Err(ReelError::Unsupported {
                            what: "y4m-colorspace-or-bit-depth",
                        });
                    }
                };
            }
            _ => {} // A (aspect) and X (extension) tags are not needed for sample access.
        }
    }
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(ReelError::Corrupt {
            what: "y4m-dimensions",
        });
    }
    let (num, den) = rate.ok_or(ReelError::Corrupt {
        what: "y4m-rate-missing",
    })?;
    let frame_rate = FrameRate::from_fps(num, den).map_err(|_| ReelError::Unsupported {
        what: "frame-rate-not-broadcast",
    })?;
    let frame_bytes = chroma
        .frame_bytes(width, height)
        .ok_or(ReelError::LimitExceeded {
            what: "frame-bytes",
        })?;
    if frame_bytes > limits.max_chunk_bytes as u64 {
        return Err(ReelError::LimitExceeded {
            what: "frame-bytes",
        });
    }
    let header_bytes = u64::try_from(end)
        .ok()
        .and_then(|e| e.checked_add(1))
        .ok_or(ReelError::LimitExceeded {
            what: "y4m-header-bytes",
        })?;
    let record = record_bytes(frame_bytes)?;
    Ok(Y4mInfo {
        width,
        height,
        ticks_per_frame: frame_rate.ticks_per_frame(),
        frame_bytes,
        header_bytes,
        record,
        complete_frames: file_len
            .saturating_sub(header_bytes)
            .checked_div(record)
            .ok_or(ReelError::Corrupt {
                what: "y4m-zero-record",
            })?,
    })
}

/// Bytes of one `FRAME` record: marker plus pixel bytes (at least the 6-byte marker).
fn record_bytes(frame_bytes: u64) -> Result<u64, ReelError> {
    frame_bytes
        .checked_add(FRAME_MARKER_LEN)
        .ok_or(ReelError::LimitExceeded {
            what: "frame-bytes",
        })
}

fn tick_span(units: u64, tpf: u64) -> Result<Ticks, ReelError> {
    units
        .checked_mul(tpf)
        .map(Ticks::new)
        .ok_or(ReelError::LimitExceeded { what: "tick-range" })
}

impl MediaProvider for Y4mProvider {
    fn descriptor(&self) -> CodecDescriptor {
        CodecDescriptor {
            id: Y4M_PROVIDER_ID,
            kind: MediaKind::Video,
            container: "y4m",
            bit_depths: &[8],
            chroma: Some("420,422,444,mono"),
            transfer: None,
            decoder_version: DECODER_VERSION,
            can_decode: true,
            can_encode: true,
        }
    }

    fn sniff(&self, head: &[u8]) -> bool {
        head.starts_with(b"YUV4MPEG2 ")
    }

    fn probe(
        &self,
        src: &mut dyn ReadSeek,
        limits: &Limits,
        cancel: &CancellationToken,
    ) -> Result<StreamLayout, ReelError> {
        check_cancel(cancel)?;
        let len = src.seek(SeekFrom::End(0))?;
        let info = parse_y4m(src, len, limits)?;
        Ok(StreamLayout {
            kind: MediaKind::Video,
            codec: Y4M_PROVIDER_ID.into(),
            width: info.width,
            height: info.height,
            ticks_per_unit: info.ticks_per_frame,
            units: info.complete_frames,
            channels: 0,
            tracks: 1,
        })
    }

    fn decode_segment(
        &self,
        src: &mut dyn ReadSeek,
        req: &SegmentRequest,
        limits: &Limits,
        cancel: &CancellationToken,
        sink: &mut dyn ChunkSink,
    ) -> Result<SegmentReceipt, ReelError> {
        check_cancel(cancel)?;
        if req.codec != Y4M_PROVIDER_ID {
            return Err(ReelError::CodecUnavailable {
                codec: req.codec.clone(),
            });
        }
        if req.output != OutputMode::Decoded {
            return Err(ReelError::Unsupported {
                what: "y4m-packets-output",
            });
        }
        if req.count == 0 {
            return Err(ReelError::Unsupported { what: "zero-count" });
        }
        let file_len = verify_grant(src, &req.grant, req.expected_revision)?;
        let info = parse_y4m(src, file_len, limits)?;
        let tpf = info.ticks_per_frame;
        let start = req
            .start_pts
            .value()
            .checked_div(tpf)
            .ok_or(ReelError::Corrupt {
                what: "y4m-zero-frame-duration",
            })?;
        let aligned = req.start_pts.value().is_multiple_of(tpf);
        let want_end = start
            .checked_add(req.count)
            .ok_or(ReelError::LimitExceeded {
                what: "frame-range",
            })?;
        // Export is exact or it fails, before any frame is read: the range must lie inside the complete
        // frames of the file (PLR-068), as in every other provider; without this check the frames before
        // the end reached the sink and the call then failed `Truncated`. A y4m header declares no frame
        // count, so a preview of a range past the last complete frame keeps streaming the frames that
        // exist and ends `Truncated { delivered }` (test `wav_and_y4m_exact_truncated_and_bounded`);
        // test `export_range_past_the_end_is_refused_before_the_first_read_for_every_provider` pins both.
        if req.purpose == Purpose::Export && want_end > info.complete_frames {
            return Err(ReelError::OutOfRange {
                requested: req.start_pts,
                first: 0,
                end: i64::try_from(tick_span(info.complete_frames, tpf)?.value())
                    .unwrap_or(i64::MAX),
            });
        }
        // ... and the start must be a whole frame.
        require_exact_start(req, aligned, || tick_span(start, tpf))?;
        let record = info.record;
        let first_byte = start
            .checked_mul(record)
            .and_then(|b| b.checked_add(info.header_bytes))
            .ok_or(ReelError::LimitExceeded {
                what: "byte-offset",
            })?;
        src.seek(SeekFrom::Start(first_byte))?;
        // The most the source can still hold at the seek position: a frame buffer is allocated only
        // when one whole record fits there, whatever the header (and a generous `Limits`) declare.
        let available = file_len.saturating_sub(first_byte);
        let frame_len =
            usize::try_from(info.frame_bytes).map_err(|_| ReelError::LimitExceeded {
                what: "frame-bytes",
            })?;

        let mut buf: Vec<u8> = Vec::new();
        // One counter drives the loop, the chunk index and the receipt, so they cannot disagree; the
        // receipt field is u32, so the 2^32-th chunk is refused instead of wrapping.
        let (mut chunks, mut marker) = (0u32, [0u8; 6]);
        // The bound of the loop below; the receipt reads the loop's counter, not this bound or the request
        // (a test can stop the loop short of it, see `model::short_loop`; test
        // `y4m_receipt_states_what_the_loop_delivered`).
        let loop_count = req.count;
        #[cfg(test)]
        let loop_count = short_loop::bound(loop_count);
        while u64::from(chunks) < loop_count {
            check_cancel(cancel)?;
            let delivered = u64::from(chunks);
            let index = start
                .checked_add(delivered)
                .ok_or(ReelError::LimitExceeded {
                    what: "frame-range",
                })?;
            read_exact_or_truncated(src, &mut marker, delivered)?;
            if &marker != FRAME_MARKER {
                return Err(if marker.starts_with(b"FRAME ") {
                    ReelError::Unsupported {
                        what: "y4m-frame-parameters",
                    }
                } else {
                    ReelError::Corrupt {
                        what: "y4m-frame-marker",
                    }
                });
            }
            if buf.len() != frame_len {
                if available < record {
                    return Err(ReelError::Truncated { delivered });
                }
                buf = vec![0u8; frame_len];
            }
            read_exact_or_truncated(src, &mut buf, delivered)?;
            chunks = chunks.checked_add(1).ok_or(ReelError::LimitExceeded {
                what: "chunk-count",
            })?;
            sink.accept(Chunk {
                kind: ChunkKind::VideoFrame,
                pts: i64::try_from(tick_span(index, tpf)?.value())
                    .map_err(|_| ReelError::LimitExceeded { what: "tick-range" })?,
                duration: Ticks::new(tpf),
                index,
                units: 1,
                sync: true,
                preroll: false,
                trim_start: 0,
                bytes: &buf,
            })?;
        }
        let delivered = u64::from(chunks);
        let delivered_end = start
            .checked_add(delivered)
            .ok_or(ReelError::LimitExceeded {
                what: "frame-range",
            })?;
        let complete = delivery_complete(req, delivered)?;
        let bytes_read = delivered
            .checked_mul(record)
            .and_then(|b| b.checked_add(info.header_bytes))
            .ok_or(ReelError::LimitExceeded {
                what: "byte-offset",
            })?;
        Ok(SegmentReceipt {
            requested: Span {
                start_pts: req.start_pts,
                end_pts: Ticks::new(
                    req.start_pts
                        .value()
                        .checked_add(tick_span(req.count, tpf)?.value())
                        .ok_or(ReelError::LimitExceeded { what: "tick-range" })?,
                ),
                units: req.count,
            },
            actual: Span {
                start_pts: tick_span(start, tpf)?,
                // The end of what was delivered (the frames the loop counted), not of what was requested.
                end_pts: tick_span(delivered_end, tpf)?,
                units: delivered,
            },
            disposition: delivery_disposition(aligned, complete),
            source: SourceKind::Original,
            proxy_note: ProxyNote::NotRequested,
            original_id: req.grant.content_id.clone(),
            proxy_id: None,
            codec: Y4M_PROVIDER_ID.into(),
            decoder_id: Y4M_PROVIDER_ID,
            decoder_version: DECODER_VERSION,
            chunks,
            bytes_read,
            complete,
            ticks_exact: true,
            video: None,
        })
    }

    fn encode_segment(
        &self,
        spec: &EncodeSpec,
        source: &mut dyn ChunkSource,
        out: &mut dyn Write,
        limits: &Limits,
        cancel: &CancellationToken,
    ) -> Result<EncodeReceipt, ReelError> {
        if spec.codec != Y4M_PROVIDER_ID {
            return Err(ReelError::CodecUnavailable {
                codec: spec.codec.clone(),
            });
        }
        let EncodeParams::Video {
            width,
            height,
            ticks_per_frame,
            chroma,
        } = spec.params
        else {
            return Err(ReelError::Unsupported {
                what: "y4m-audio-params",
            });
        };
        let chroma = Chroma::from_name(chroma).ok_or(ReelError::Unsupported {
            what: "y4m-encode-chroma",
        })?;
        if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
            return Err(ReelError::Corrupt {
                what: "y4m-dimensions",
            });
        }
        let (frame_rate, normalization) =
            FrameRate::from_import(ticks_per_frame).map_err(|_| ReelError::Unsupported {
                what: "frame-rate-not-broadcast",
            })?;
        if normalization.changed {
            return Err(ReelError::Unsupported {
                what: "frame-rate-not-broadcast",
            });
        }
        let (fps_num, fps_den) = frame_rate.fps_fraction();
        let frame_bytes = chroma
            .frame_bytes(width, height)
            .filter(|b| usize::try_from(*b).is_ok_and(|b| b <= limits.max_chunk_bytes))
            .ok_or(ReelError::LimitExceeded {
                what: "frame-bytes",
            })?;
        let frame_len = usize::try_from(frame_bytes).map_err(|_| ReelError::LimitExceeded {
            what: "frame-bytes",
        })?;
        let header = format!(
            "YUV4MPEG2 W{width} H{height} F{}:{} Ip A1:1 C{}\n",
            fps_num,
            fps_den,
            if chroma == Chroma::C420 {
                "420jpeg"
            } else {
                chroma.name()
            }
        );
        out.write_all(header.as_bytes())?;
        let mut bytes_written =
            u64::try_from(header.len()).map_err(|_| ReelError::LimitExceeded {
                what: "bytes-written",
            })?;
        let (mut buf, mut written, mut canceled) = (Vec::new(), 0u64, false);
        loop {
            if check_cancel(cancel).is_err() {
                canceled = true;
                break;
            }
            buf.clear();
            let Some(units) = source.next_chunk(&mut buf)? else {
                break;
            };
            if check_cancel(cancel).is_err() {
                canceled = true;
                break;
            }
            // `units` comes from the source: a product that wraps must not be able to equal the
            // buffer length (a lying count would otherwise be reported as written frames).
            let expected = units.checked_mul(frame_bytes);
            if units == 0 || expected.is_none_or(|e| u64::try_from(buf.len()) != Ok(e)) {
                return Err(ReelError::Corrupt {
                    what: "chunk-size-mismatch",
                });
            }
            let new_total = written
                .checked_add(units)
                .filter(|total| *total <= spec.total_units)
                .ok_or(ReelError::LimitExceeded {
                    what: "source-exceeds-declared",
                })?;
            for frame in buf.chunks(frame_len) {
                out.write_all(FRAME_MARKER)?;
                out.write_all(frame)?;
                bytes_written = u64::try_from(frame.len())
                    .ok()
                    .and_then(|f| f.checked_add(FRAME_MARKER_LEN))
                    .and_then(|record| bytes_written.checked_add(record))
                    .ok_or(ReelError::LimitExceeded {
                        what: "bytes-written",
                    })?;
            }
            written = new_total;
        }
        Ok(EncodeReceipt {
            codec: Y4M_PROVIDER_ID.into(),
            complete: !canceled && written == spec.total_units,
            units_written: written,
            units_declared: spec.total_units,
            bytes_written,
            canceled,
        })
    }
}
