//! YUV4MPEG2 (Y4M) provider: license-free raw video, 8-bit 4:2:0 / 4:2:2 / 4:4:4 / mono, progressive,
//! plain `FRAME\n` records. Decode streams ONE frame at a time into a reused buffer; peak memory is one
//! frame plus the header. Frame rate must map to an exact broadcast tick duration (`FrameRate`).
use crate::error::{ReelError, check_cancel};
use crate::model::*;
use hsk_studio_accord::{CancellationToken, FrameRate, TICKS_PER_SECOND, Ticks};
use std::io::{SeekFrom, Write};

pub const Y4M_PROVIDER_ID: &str = "y4m";
const DECODER_VERSION: &str = "1";
const MAX_HEADER_BYTES: usize = 4096;
const MAX_DIMENSION: u32 = 65_536;
const FRAME_MARKER: &[u8; 6] = b"FRAME\n";

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
    /// Whole `FRAME` records present in the file.
    complete_frames: u64,
    ticks_exact: bool,
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
            filled += n;
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
    let ticks = u128::from(TICKS_PER_SECOND) * u128::from(den);
    if !ticks.is_multiple_of(u128::from(num)) {
        return Err(ReelError::Unsupported {
            what: "frame-rate-not-tick-exact",
        });
    }
    let ticks = u64::try_from(ticks / u128::from(num)).map_err(|_| ReelError::Unsupported {
        what: "frame-rate-not-broadcast",
    })?;
    let (frame_rate, normalization) =
        FrameRate::from_import(ticks).map_err(|_| ReelError::Unsupported {
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
    let header_bytes = end as u64 + 1;
    let record = frame_bytes + FRAME_MARKER.len() as u64;
    Ok(Y4mInfo {
        width,
        height,
        ticks_per_frame: frame_rate.ticks_per_frame(),
        frame_bytes,
        header_bytes,
        complete_frames: file_len.saturating_sub(header_bytes) / record,
        ticks_exact: !normalization.changed,
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
        let start = req.start_pts.value() / tpf;
        let aligned = req.start_pts.value().is_multiple_of(tpf);
        let want_end = start
            .checked_add(req.count)
            .ok_or(ReelError::LimitExceeded {
                what: "frame-range",
            })?;
        let record = info.frame_bytes + FRAME_MARKER.len() as u64;
        let first_byte = start
            .checked_mul(record)
            .and_then(|b| b.checked_add(info.header_bytes))
            .ok_or(ReelError::LimitExceeded {
                what: "byte-offset",
            })?;
        src.seek(SeekFrom::Start(first_byte))?;

        let mut buf = vec![0u8; info.frame_bytes as usize];
        let (mut delivered, mut marker) = (0u64, [0u8; 6]);
        while start + delivered < want_end {
            check_cancel(cancel)?;
            let index = start + delivered;
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
            read_exact_or_truncated(src, &mut buf, delivered)?;
            sink.accept(Chunk {
                kind: ChunkKind::VideoFrame,
                pts: i64::try_from(tick_span(index, tpf)?.value())
                    .map_err(|_| ReelError::LimitExceeded { what: "tick-range" })?,
                duration: Ticks::new(tpf),
                index,
                units: 1,
                sync: true,
                preroll: false,
                bytes: &buf,
            })?;
            delivered += 1;
        }
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
                end_pts: tick_span(want_end, tpf)?,
                units: delivered,
            },
            disposition: if aligned {
                Disposition::Exact
            } else {
                Disposition::Approximate
            },
            source: SourceKind::Original,
            proxy_note: ProxyNote::NotRequested,
            original_id: req.grant.content_id.clone(),
            proxy_id: None,
            codec: Y4M_PROVIDER_ID.into(),
            decoder_id: Y4M_PROVIDER_ID,
            decoder_version: DECODER_VERSION,
            chunks: delivered as u32,
            bytes_read: info.header_bytes + delivered * record,
            complete: true,
            ticks_exact: info.ticks_exact,
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
        FrameRate::from_import(ticks_per_frame).map_err(|_| ReelError::Unsupported {
            what: "frame-rate-not-broadcast",
        })?;
        let frame_bytes = chroma
            .frame_bytes(width, height)
            .filter(|b| *b <= limits.max_chunk_bytes as u64)
            .ok_or(ReelError::LimitExceeded {
                what: "frame-bytes",
            })?;
        let g = gcd(TICKS_PER_SECOND, ticks_per_frame);
        let header = format!(
            "YUV4MPEG2 W{width} H{height} F{}:{} Ip A1:1 C{}\n",
            TICKS_PER_SECOND / g,
            ticks_per_frame / g,
            if chroma == Chroma::C420 {
                "420jpeg"
            } else {
                chroma.name()
            }
        );
        out.write_all(header.as_bytes())?;
        let mut bytes_written = header.len() as u64;
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
            if units == 0 || buf.len() as u64 != units * frame_bytes {
                return Err(ReelError::Corrupt {
                    what: "chunk-size-mismatch",
                });
            }
            if written + units > spec.total_units {
                return Err(ReelError::LimitExceeded {
                    what: "source-exceeds-declared",
                });
            }
            for frame in buf.chunks(frame_bytes as usize) {
                out.write_all(FRAME_MARKER)?;
                out.write_all(frame)?;
                bytes_written += FRAME_MARKER.len() as u64 + frame.len() as u64;
            }
            written += units;
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

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}
