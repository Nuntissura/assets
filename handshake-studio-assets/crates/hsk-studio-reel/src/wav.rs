//! WAV / RIFF PCM provider: bounded streaming sample-range decode and exact-header encode.
//! Integer PCM (8/16/24/32 bit) and IEEE float (32/64 bit); samples are delivered in file byte order.
//! Non-PCM tags (ADPCM, MP3-in-WAV, A-law ...) are a typed `CodecUnavailable` naming the tag.
//!
//! Every chunk size, offset, sample count and duration taken from the file or the caller uses checked
//! arithmetic and a typed error (V4-01); `v4_hardening.rs` proves the u32-max and aggregate cases.
#![deny(clippy::arithmetic_side_effects)]
use crate::error::{ReelError, check_cancel};
use crate::model::*;
use crate::time::exact_ticks_per_unit;
use hsk_studio_accord::{CancellationToken, Ticks};
use std::io::{SeekFrom, Write};

pub const WAV_PROVIDER_ID: &str = "wav-pcm";
const DECODER_VERSION: &str = "1";
const MAX_CHUNKS_BEFORE_DATA: u32 = 256;
const MAX_FMT_BYTES: u32 = 64;
/// Largest data chunk the 32-bit RIFF fields can describe with the 36-byte header and pad byte to spare.
const MAX_ENCODE_DATA_BYTES: u64 = u32::MAX as u64 - 64;

#[derive(Debug, Clone, Copy)]
pub struct WavProvider;

#[derive(Debug, Clone, Copy)]
struct WavInfo {
    channels: u16,
    rate: u32,
    block_align: u16,
    data_offset: u64,
    data_declared: u64,
    data_available: u64,
    header_bytes: u64,
}

impl WavInfo {
    // `block_align` is validated non-zero in `parse_wav`; a zero here would mean an empty stream, never a
    // bogus count.
    fn declared_samples(&self) -> u64 {
        self.data_declared
            .checked_div(u64::from(self.block_align))
            .unwrap_or(0)
    }
    fn available_samples(&self) -> u64 {
        self.data_available
            .min(self.data_declared)
            .checked_div(u64::from(self.block_align))
            .unwrap_or(0)
    }
}

fn le_array<const N: usize>(b: &[u8], at: usize) -> Result<[u8; N], ReelError> {
    at.checked_add(N)
        .and_then(|end| b.get(at..end))
        .and_then(|bytes| <[u8; N]>::try_from(bytes).ok())
        .ok_or(ReelError::Corrupt { what: "wav-fmt" })
}
fn le16(b: &[u8], at: usize) -> Result<u16, ReelError> {
    le_array::<2>(b, at).map(u16::from_le_bytes)
}
fn le32(b: &[u8], at: usize) -> Result<u32, ReelError> {
    le_array::<4>(b, at).map(u32::from_le_bytes)
}

/// `offset + by` for a position in the file: a sum that does not fit u64 is a LimitExceeded, not a wrap.
fn advance(offset: u64, by: u64) -> Result<u64, ReelError> {
    offset
        .checked_add(by)
        .ok_or(ReelError::LimitExceeded { what: "wav-offset" })
}

fn parse_wav(
    src: &mut dyn ReadSeek,
    file_len: u64,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<WavInfo, ReelError> {
    let mut head = [0u8; 12];
    read_exact_structural(src, &mut head, "wav-riff-header")?;
    match &head[0..4] {
        b"RIFF" => {}
        b"RF64" | b"BW64" => {
            return Err(ReelError::Unsupported { what: "rf64-wav" });
        }
        _ => {
            return Err(ReelError::ImportUnsupported {
                detected: "not-riff".into(),
            });
        }
    }
    if &head[8..12] != b"WAVE" {
        return Err(ReelError::ImportUnsupported {
            detected: "riff-non-wave".into(),
        });
    }
    let mut read = 12u64;
    let mut fmt: Option<(u16, u16, u32, u16, u16)> = None; // (tag, channels, rate, block_align, bits)
    for _ in 0..MAX_CHUNKS_BEFORE_DATA {
        check_cancel(cancel)?;
        let mut hdr = [0u8; 8];
        read_exact_structural(src, &mut hdr, "wav-chunk-header")?;
        read = advance(read, 8)?;
        let id = [hdr[0], hdr[1], hdr[2], hdr[3]];
        let size = le32(&hdr, 4)?;
        if read > limits.max_probe_bytes {
            return Err(ReelError::LimitExceeded {
                what: "probe-bytes",
            });
        }
        match &id {
            b"fmt " => {
                if !(16..=MAX_FMT_BYTES).contains(&size) {
                    return Err(ReelError::Corrupt {
                        what: "wav-fmt-size",
                    });
                }
                let mut buf = [0u8; MAX_FMT_BYTES as usize];
                let body = &mut buf[..size as usize];
                read_exact_structural(src, body, "wav-fmt")?;
                read = advance(read, u64::from(size))?;
                let mut tag = le16(body, 0)?;
                if tag == 0xFFFE {
                    if size < 40 {
                        return Err(ReelError::Corrupt {
                            what: "wav-extensible-size",
                        });
                    }
                    tag = le16(body, 24)?;
                }
                fmt = Some((
                    tag,
                    le16(body, 2)?,
                    le32(body, 4)?,
                    le16(body, 12)?,
                    le16(body, 14)?,
                ));
                if size % 2 == 1 {
                    src.seek(SeekFrom::Current(1))?;
                    read = advance(read, 1)?;
                }
            }
            b"data" => {
                let (tag, channels, rate, block_align, bits) = fmt.ok_or(ReelError::Corrupt {
                    what: "wav-data-before-fmt",
                })?;
                let float = match tag {
                    1 => false,
                    3 => true,
                    other => {
                        return Err(ReelError::CodecUnavailable {
                            codec: format!("wav-tag-0x{other:04x}"),
                        });
                    }
                };
                let supported = if float {
                    matches!(bits, 32 | 64)
                } else {
                    matches!(bits, 8 | 16 | 24 | 32)
                };
                if !supported {
                    return Err(ReelError::Unsupported {
                        what: "wav-bit-depth",
                    });
                }
                if channels == 0 || channels > 64 || rate == 0 {
                    return Err(ReelError::Corrupt {
                        what: "wav-channels-or-rate",
                    });
                }
                if Some(u32::from(block_align))
                    != u32::from(channels).checked_mul(u32::from(bits / 8))
                {
                    return Err(ReelError::Corrupt {
                        what: "wav-block-align",
                    });
                }
                let data_offset = read;
                let available = file_len.saturating_sub(data_offset);
                let declared = if size == u32::MAX {
                    available
                } else {
                    u64::from(size)
                };
                return Ok(WavInfo {
                    channels,
                    rate,
                    block_align,
                    data_offset,
                    data_declared: declared,
                    data_available: available,
                    header_bytes: read,
                });
            }
            _ => {
                let skip = advance(u64::from(size), u64::from(size % 2))?;
                if read.saturating_add(skip) > file_len {
                    return Err(ReelError::Corrupt {
                        what: "wav-chunk-exceeds-file",
                    });
                }
                let relative = i64::try_from(skip)
                    .map_err(|_| ReelError::LimitExceeded { what: "wav-offset" })?;
                src.seek(SeekFrom::Current(relative))?;
                read = advance(read, skip)?;
            }
        }
    }
    Err(ReelError::LimitExceeded {
        what: "wav-chunks-before-data",
    })
}

fn tick_span(units: u64, tps: u64) -> Result<Ticks, ReelError> {
    units
        .checked_mul(tps)
        .map(Ticks::new)
        .ok_or(ReelError::LimitExceeded { what: "tick-range" })
}

impl MediaProvider for WavProvider {
    fn descriptor(&self) -> CodecDescriptor {
        CodecDescriptor {
            id: WAV_PROVIDER_ID,
            kind: MediaKind::Audio,
            container: "wav",
            bit_depths: &[8, 16, 24, 32, 64],
            chroma: None,
            transfer: None,
            decoder_version: DECODER_VERSION,
            can_decode: true,
            can_encode: true,
        }
    }

    fn sniff(&self, head: &[u8]) -> bool {
        head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WAVE"
    }

    fn probe(
        &self,
        src: &mut dyn ReadSeek,
        limits: &Limits,
        cancel: &CancellationToken,
    ) -> Result<StreamLayout, ReelError> {
        let len = src.seek(SeekFrom::End(0))?;
        src.seek(SeekFrom::Start(0))?;
        let info = parse_wav(src, len, limits, cancel)?;
        let tps = exact_ticks_per_unit(info.rate).ok_or(ReelError::Unsupported {
            what: "sample-rate-not-tick-exact",
        })?;
        Ok(StreamLayout {
            kind: MediaKind::Audio,
            codec: WAV_PROVIDER_ID.into(),
            width: 0,
            height: 0,
            ticks_per_unit: tps,
            units: info.declared_samples(),
            channels: info.channels,
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
        if req.codec != WAV_PROVIDER_ID {
            return Err(ReelError::CodecUnavailable {
                codec: req.codec.clone(),
            });
        }
        if req.output != OutputMode::Decoded {
            return Err(ReelError::Unsupported {
                what: "wav-packets-output",
            });
        }
        if req.count == 0 {
            return Err(ReelError::Unsupported { what: "zero-count" });
        }
        let file_len = verify_grant(src, &req.grant, req.expected_revision)?;
        let info = parse_wav(src, file_len, limits, cancel)?;
        let tps = exact_ticks_per_unit(info.rate).ok_or(ReelError::Unsupported {
            what: "sample-rate-not-tick-exact",
        })?;
        let block = usize::from(info.block_align);
        let start = req
            .start_pts
            .value()
            .checked_div(tps)
            .ok_or(ReelError::Corrupt {
                what: "wav-zero-sample-duration",
            })?;
        let aligned = req.start_pts.value().is_multiple_of(tps);
        let want_end = start
            .checked_add(req.count)
            .ok_or(ReelError::LimitExceeded {
                what: "sample-range",
            })?;
        let declared = info.declared_samples();
        if want_end > declared {
            return Err(ReelError::OutOfRange {
                requested: req.start_pts,
                first: 0,
                end: i64::try_from(tick_span(declared, tps)?.value()).unwrap_or(i64::MAX),
            });
        }
        // Export is exact or it fails, before any sample is read: the start must be a whole sample.
        require_exact_start(req, aligned, || tick_span(start, tps))?;
        let available = info.available_samples();
        let deliver_end = want_end.min(available);
        let chunk_samples = limits
            .max_chunk_bytes
            .checked_div(block)
            .and_then(|n| u64::try_from(n).ok())
            .unwrap_or(0);
        if chunk_samples == 0 {
            return Err(ReelError::LimitExceeded {
                what: "chunk-smaller-than-sample",
            });
        }
        let first_byte = start
            .checked_mul(u64::from(info.block_align))
            .and_then(|b| b.checked_add(info.data_offset))
            .ok_or(ReelError::LimitExceeded {
                what: "byte-offset",
            })?;
        src.seek(SeekFrom::Start(first_byte))?;

        let slice_bytes = |samples: u64| -> Result<usize, ReelError> {
            usize::try_from(samples)
                .ok()
                .and_then(|n| n.checked_mul(block))
                .ok_or(ReelError::LimitExceeded {
                    what: "chunk-bytes",
                })
        };
        let cap = slice_bytes(chunk_samples.min(deliver_end.saturating_sub(start)))?;
        let mut buf = vec![0u8; cap];
        let mut cursor = start;
        let (mut chunks, mut payload) = (0u32, 0u64);
        while cursor < deliver_end {
            check_cancel(cancel)?;
            let remaining = deliver_end.saturating_sub(cursor);
            let n = chunk_samples.min(remaining);
            let bytes = slice_bytes(n)?;
            let done = cursor.saturating_sub(start);
            read_exact_or_truncated(src, &mut buf[..bytes], done)?;
            sink.accept(Chunk {
                kind: ChunkKind::AudioSamples,
                pts: i64::try_from(tick_span(cursor, tps)?.value())
                    .map_err(|_| ReelError::LimitExceeded { what: "tick-range" })?,
                duration: tick_span(n, tps)?,
                index: cursor,
                units: n,
                sync: true,
                preroll: false,
                trim_start: 0,
                bytes: &buf[..bytes],
            })?;
            cursor = advance(cursor, n)?;
            chunks = chunks.checked_add(1).ok_or(ReelError::LimitExceeded {
                what: "chunk-count",
            })?;
            payload = u64::try_from(bytes)
                .ok()
                .and_then(|b| payload.checked_add(b))
                .ok_or(ReelError::LimitExceeded { what: "bytes-read" })?;
        }
        let bytes_read = advance(info.header_bytes, payload)?;
        let delivered = deliver_end.saturating_sub(start);
        if deliver_end < want_end {
            return Err(ReelError::Truncated { delivered });
        }
        Ok(SegmentReceipt {
            requested: Span {
                start_pts: req.start_pts,
                end_pts: Ticks::new(
                    req.start_pts
                        .value()
                        .checked_add(tick_span(req.count, tps)?.value())
                        .ok_or(ReelError::LimitExceeded { what: "tick-range" })?,
                ),
                units: req.count,
            },
            actual: Span {
                start_pts: tick_span(start, tps)?,
                end_pts: tick_span(deliver_end, tps)?,
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
            codec: WAV_PROVIDER_ID.into(),
            decoder_id: WAV_PROVIDER_ID,
            decoder_version: DECODER_VERSION,
            chunks,
            bytes_read,
            complete: true,
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
        if spec.codec != WAV_PROVIDER_ID {
            return Err(ReelError::CodecUnavailable {
                codec: spec.codec.clone(),
            });
        }
        let EncodeParams::Audio {
            rate,
            channels,
            bits,
            float,
        } = spec.params
        else {
            return Err(ReelError::Unsupported {
                what: "wav-video-params",
            });
        };
        let ok_bits = if float {
            matches!(bits, 32 | 64)
        } else {
            matches!(bits, 8 | 16 | 24 | 32)
        };
        if !ok_bits || channels == 0 || channels > 64 || rate == 0 {
            return Err(ReelError::Unsupported {
                what: "wav-encode-format",
            });
        }
        let block =
            u32::from(channels)
                .checked_mul(u32::from(bits / 8))
                .ok_or(ReelError::Unsupported {
                    what: "wav-encode-format",
                })?;
        let data_bytes = spec
            .total_units
            .checked_mul(u64::from(block))
            .filter(|b| *b <= MAX_ENCODE_DATA_BYTES)
            .ok_or(ReelError::LimitExceeded {
                what: "wav-data-size",
            })?;
        let pad = data_bytes % 2;
        let size_error = || ReelError::LimitExceeded {
            what: "wav-data-size",
        };
        // RIFF size = 36 header bytes after the 8-byte preamble + data + pad byte.
        let riff_size = data_bytes
            .checked_add(pad)
            .and_then(|d| d.checked_add(36))
            .and_then(|total| u32::try_from(total).ok())
            .ok_or_else(size_error)?;
        let data_size = u32::try_from(data_bytes).map_err(|_| size_error())?;
        let byte_rate = rate.checked_mul(block).ok_or(ReelError::LimitExceeded {
            what: "wav-byte-rate",
        })?;
        let block_align = u16::try_from(block).map_err(|_| ReelError::Unsupported {
            what: "wav-encode-format",
        })?;
        let mut header = Vec::with_capacity(44);
        header.extend_from_slice(b"RIFF");
        header.extend_from_slice(&riff_size.to_le_bytes());
        header.extend_from_slice(b"WAVEfmt ");
        header.extend_from_slice(&16u32.to_le_bytes());
        header.extend_from_slice(&(if float { 3u16 } else { 1u16 }).to_le_bytes());
        header.extend_from_slice(&channels.to_le_bytes());
        header.extend_from_slice(&rate.to_le_bytes());
        header.extend_from_slice(&byte_rate.to_le_bytes());
        header.extend_from_slice(&block_align.to_le_bytes());
        header.extend_from_slice(&bits.to_le_bytes());
        header.extend_from_slice(b"data");
        header.extend_from_slice(&data_size.to_le_bytes());
        out.write_all(&header)?;
        let mut bytes_written =
            u64::try_from(header.len()).map_err(|_| ReelError::LimitExceeded {
                what: "bytes-written",
            })?;

        let mut buf = Vec::new();
        let mut written = 0u64;
        let mut canceled = false;
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
            // buffer length.
            let expected = units.checked_mul(u64::from(block));
            if expected.is_none_or(|e| u64::try_from(buf.len()) != Ok(e)) {
                return Err(ReelError::Corrupt {
                    what: "chunk-size-mismatch",
                });
            }
            if buf.len() > limits.max_chunk_bytes {
                return Err(ReelError::LimitExceeded {
                    what: "chunk-bytes",
                });
            }
            let new_total = written
                .checked_add(units)
                .filter(|total| *total <= spec.total_units)
                .ok_or(ReelError::LimitExceeded {
                    what: "source-exceeds-declared",
                })?;
            out.write_all(&buf)?;
            written = new_total;
            bytes_written = u64::try_from(buf.len())
                .ok()
                .and_then(|b| bytes_written.checked_add(b))
                .ok_or(ReelError::LimitExceeded {
                    what: "bytes-written",
                })?;
        }
        let complete = !canceled && written == spec.total_units;
        if complete && pad == 1 {
            out.write_all(&[0])?;
            bytes_written = advance(bytes_written, 1)?;
        }
        Ok(EncodeReceipt {
            codec: WAV_PROVIDER_ID.into(),
            complete,
            units_written: written,
            units_declared: spec.total_units,
            bytes_written,
            canceled,
        })
    }
}
