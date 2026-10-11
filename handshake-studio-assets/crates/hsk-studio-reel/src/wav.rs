//! WAV / RIFF PCM provider: bounded streaming sample-range decode and exact-header encode.
//! Integer PCM (8/16/24/32 bit) and IEEE float (32/64 bit); samples are delivered in file byte order.
//! Non-PCM tags (ADPCM, MP3-in-WAV, A-law ...) are a typed `CodecUnavailable` naming the tag.
use crate::error::{ReelError, check_cancel};
use crate::model::*;
use crate::time::exact_ticks_per_unit;
use hsk_studio_accord::{CancellationToken, Ticks};
use std::io::{SeekFrom, Write};

pub const WAV_PROVIDER_ID: &str = "wav-pcm";
const DECODER_VERSION: &str = "1";
const MAX_CHUNKS_BEFORE_DATA: u32 = 256;
const MAX_FMT_BYTES: u32 = 64;

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
    fn declared_samples(&self) -> u64 {
        self.data_declared / u64::from(self.block_align)
    }
    fn available_samples(&self) -> u64 {
        self.data_available.min(self.data_declared) / u64::from(self.block_align)
    }
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
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
        read += 8;
        let id = [hdr[0], hdr[1], hdr[2], hdr[3]];
        let size = le32(&hdr, 4);
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
                read += u64::from(size);
                let mut tag = le16(body, 0);
                if tag == 0xFFFE {
                    if size < 40 {
                        return Err(ReelError::Corrupt {
                            what: "wav-extensible-size",
                        });
                    }
                    tag = le16(body, 24);
                }
                fmt = Some((
                    tag,
                    le16(body, 2),
                    le32(body, 4),
                    le16(body, 12),
                    le16(body, 14),
                ));
                if size % 2 == 1 {
                    src.seek(SeekFrom::Current(1))?;
                    read += 1;
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
                if u32::from(block_align) != u32::from(channels) * u32::from(bits / 8) {
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
                let skip = u64::from(size) + u64::from(size % 2);
                if read.saturating_add(skip) > file_len {
                    return Err(ReelError::Corrupt {
                        what: "wav-chunk-exceeds-file",
                    });
                }
                src.seek(SeekFrom::Current(skip as i64))?;
                read += skip;
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
        let start = req.start_pts.value() / tps;
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
        let available = info.available_samples();
        let deliver_end = want_end.min(available);
        let chunk_samples = (limits.max_chunk_bytes / block) as u64;
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

        let cap = chunk_samples.min(deliver_end.saturating_sub(start)) as usize * block;
        let mut buf = vec![0u8; cap];
        let mut cursor = start;
        let (mut chunks, mut payload) = (0u32, 0u64);
        while cursor < deliver_end {
            check_cancel(cancel)?;
            let n = chunk_samples.min(deliver_end - cursor);
            let bytes = n as usize * block;
            read_exact_or_truncated(src, &mut buf[..bytes], cursor - start)?;
            sink.accept(Chunk {
                kind: ChunkKind::AudioSamples,
                pts: i64::try_from(tick_span(cursor, tps)?.value())
                    .map_err(|_| ReelError::LimitExceeded { what: "tick-range" })?,
                duration: tick_span(n, tps)?,
                index: cursor,
                units: n,
                sync: true,
                preroll: false,
                bytes: &buf[..bytes],
            })?;
            cursor += n;
            chunks += 1;
            payload += bytes as u64;
        }
        let delivered = deliver_end - start;
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
            bytes_read: info.header_bytes + payload,
            complete: true,
            ticks_exact: true,
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
        let block = u32::from(channels) * u32::from(bits / 8);
        let data_bytes = spec
            .total_units
            .checked_mul(u64::from(block))
            .filter(|b| *b <= u64::from(u32::MAX) - 64)
            .ok_or(ReelError::LimitExceeded {
                what: "wav-data-size",
            })?;
        let pad = data_bytes % 2;
        let mut header = Vec::with_capacity(44);
        header.extend_from_slice(b"RIFF");
        header.extend_from_slice(&((36 + data_bytes + pad) as u32).to_le_bytes());
        header.extend_from_slice(b"WAVEfmt ");
        header.extend_from_slice(&16u32.to_le_bytes());
        header.extend_from_slice(&(if float { 3u16 } else { 1u16 }).to_le_bytes());
        header.extend_from_slice(&channels.to_le_bytes());
        header.extend_from_slice(&rate.to_le_bytes());
        header.extend_from_slice(&rate.saturating_mul(block).to_le_bytes());
        header.extend_from_slice(&(block as u16).to_le_bytes());
        header.extend_from_slice(&bits.to_le_bytes());
        header.extend_from_slice(b"data");
        header.extend_from_slice(&(data_bytes as u32).to_le_bytes());
        out.write_all(&header)?;
        let mut bytes_written = header.len() as u64;

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
            if buf.len() as u64 != units * u64::from(block) {
                return Err(ReelError::Corrupt {
                    what: "chunk-size-mismatch",
                });
            }
            if buf.len() > limits.max_chunk_bytes {
                return Err(ReelError::LimitExceeded {
                    what: "chunk-bytes",
                });
            }
            if written + units > spec.total_units {
                return Err(ReelError::LimitExceeded {
                    what: "source-exceeds-declared",
                });
            }
            out.write_all(&buf)?;
            written += units;
            bytes_written += buf.len() as u64;
        }
        let complete = !canceled && written == spec.total_units;
        if complete && pad == 1 {
            out.write_all(&[0])?;
            bytes_written += 1;
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
