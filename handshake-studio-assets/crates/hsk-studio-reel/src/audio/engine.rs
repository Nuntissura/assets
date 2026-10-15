//! Shared delivery loop for decoded compressed audio: a `FrameSource` yields decoded blocks (interleaved
//! f32), the loop drops the leading encoder delay and everything before the requested start, and hands
//! the sink bounded chunks of interleaved little-endian f32 (`ChunkKind::AudioSamples`). Receipts follow
//! the WAV provider's rules: export is exact or it fails, a range past a declared end is
//! `MEDIA_OUT_OF_RANGE` before the first payload read, and `complete`, `actual.units` and the actual end
//! come from what the loop delivered.
#![deny(clippy::arithmetic_side_effects)]
use crate::error::{ReelError, check_cancel};
use crate::model::*;
use crate::time::exact_ticks_per_unit;
use hsk_studio_accord::{CancellationToken, Ticks};

/// Decoded blocks of one audio stream, in presentation order.
pub(crate) trait FrameSource {
    /// Decode the next block and append its interleaved samples to `out`; `Ok(None)` at the end of the
    /// stream. Returns the frames (samples per channel) appended.
    fn next_block(
        &mut self,
        out: &mut Vec<f32>,
        cancel: &CancellationToken,
    ) -> Result<Option<usize>, ReelError>;
    /// Source bytes read so far (payload and structure).
    fn bytes_read(&self) -> u64;
}

/// What the loop needs to know about the stream before it reads any payload.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StreamShape {
    pub(crate) rate: u32,
    pub(crate) channels: u16,
    /// Presented frames (after `skip` and end trimming) when the container declares them.
    pub(crate) total: Option<u64>,
    /// Leading decoded frames that are not presented (encoder delay / pre-skip).
    pub(crate) skip: u64,
}

pub(crate) struct Ids {
    pub(crate) codec: String,
    pub(crate) decoder_id: &'static str,
    pub(crate) decoder_version: &'static str,
}

/// Bytes of one interleaved f32 frame.
pub(crate) fn frame_bytes(channels: u16) -> usize {
    usize::from(channels).saturating_mul(4)
}

fn tick_span(units: u64, tps: u64) -> Result<Ticks, ReelError> {
    units
        .checked_mul(tps)
        .map(Ticks::new)
        .ok_or(ReelError::LimitExceeded { what: "tick-range" })
}

/// Check what the loop needs before any payload is read: a tick-exact rate and the requested range.
/// Returns (ticks per frame, first frame, end frame, start aligned).
pub(crate) fn plan(
    shape: &StreamShape,
    req: &SegmentRequest,
) -> Result<(u64, u64, u64, bool), ReelError> {
    if req.count == 0 {
        return Err(ReelError::Unsupported { what: "zero-count" });
    }
    let tps = exact_ticks_per_unit(shape.rate).ok_or(ReelError::Unsupported {
        what: "sample-rate-not-tick-exact",
    })?;
    let start = req
        .start_pts
        .value()
        .checked_div(tps)
        .ok_or(ReelError::Corrupt {
            what: "audio-zero-sample-duration",
        })?;
    let aligned = req.start_pts.value().is_multiple_of(tps);
    let end = start
        .checked_add(req.count)
        .ok_or(ReelError::LimitExceeded {
            what: "sample-range",
        })?;
    if let Some(total) = shape.total
        && end > total
    {
        return Err(ReelError::OutOfRange {
            requested: req.start_pts,
            first: 0,
            end: i64::try_from(tick_span(total, tps)?.value()).unwrap_or(i64::MAX),
        });
    }
    require_exact_start(req, aligned, || tick_span(start, tps))?;
    Ok((tps, start, end, aligned))
}

/// Deliver frames `[start, start + req.count)` of the presented stream.
#[allow(clippy::too_many_arguments)]
pub(crate) fn deliver(
    source: &mut dyn FrameSource,
    shape: &StreamShape,
    ids: Ids,
    req: &SegmentRequest,
    limits: &Limits,
    cancel: &CancellationToken,
    sink: &mut dyn ChunkSink,
) -> Result<SegmentReceipt, ReelError> {
    let (tps, start, want_end, aligned) = plan(shape, req)?;
    let channels = usize::from(shape.channels);
    let fbytes = frame_bytes(shape.channels);
    let chunk_frames = limits.max_chunk_bytes.checked_div(fbytes).unwrap_or(0);
    if chunk_frames == 0 {
        return Err(ReelError::LimitExceeded {
            what: "chunk-smaller-than-sample",
        });
    }
    // Decoded-frame position (before skip) of the first sample in `pending`.
    let mut decoded_pos: u64 = 0;
    // Leading decoded frames to drop: the encoder delay plus everything before the requested start.
    let drop_until = shape
        .skip
        .checked_add(start)
        .ok_or(ReelError::LimitExceeded {
            what: "sample-range",
        })?;
    let loop_end = want_end;
    #[cfg(test)]
    let loop_end = short_loop::bound(loop_end);
    let mut cursor = start;
    let mut pending: Vec<f32> = Vec::new();
    let mut block: Vec<f32> = Vec::new();
    let mut bytes = Vec::new();
    let mut chunks = 0u32;
    let mut ended = false;
    while cursor < loop_end {
        check_cancel(cancel)?;
        // Fill `pending` with at least one chunk (or to the end of the stream).
        let missing_frames = loop_end.saturating_sub(cursor);
        let want_frames = missing_frames.min(chunk_frames as u64);
        while !ended && (pending.len().checked_div(channels).unwrap_or(0) as u64) < want_frames {
            block.clear();
            match source.next_block(&mut block, cancel) {
                Ok(Some(frames)) => {
                    if block.len() != frames.saturating_mul(channels) {
                        return Err(ReelError::Corrupt {
                            what: "audio-block-shape",
                        });
                    }
                    let block_start = decoded_pos;
                    decoded_pos =
                        decoded_pos
                            .checked_add(frames as u64)
                            .ok_or(ReelError::LimitExceeded {
                                what: "sample-range",
                            })?;
                    if decoded_pos <= drop_until {
                        continue;
                    }
                    let skip_frames = drop_until.saturating_sub(block_start);
                    let from = usize::try_from(skip_frames)
                        .ok()
                        .and_then(|f| f.checked_mul(channels))
                        .ok_or(ReelError::LimitExceeded {
                            what: "sample-range",
                        })?;
                    pending.extend_from_slice(block.get(from..).unwrap_or(&[]));
                }
                Ok(None) => ended = true,
                Err(ReelError::Truncated { .. }) => {
                    return Err(ReelError::Truncated {
                        delivered: cursor.saturating_sub(start),
                    });
                }
                Err(e) => return Err(e),
            }
        }
        let available = pending.len().checked_div(channels).unwrap_or(0) as u64;
        let n = want_frames.min(available);
        if n == 0 {
            break;
        }
        let values = usize::try_from(n)
            .ok()
            .and_then(|f| f.checked_mul(channels))
            .ok_or(ReelError::LimitExceeded {
                what: "chunk-bytes",
            })?;
        bytes.clear();
        for v in pending.drain(..values) {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
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
            bytes: &bytes,
        })?;
        cursor = cursor.checked_add(n).ok_or(ReelError::LimitExceeded {
            what: "sample-range",
        })?;
        chunks = chunks.checked_add(1).ok_or(ReelError::LimitExceeded {
            what: "chunk-count",
        })?;
    }
    let delivered = cursor.saturating_sub(start);
    if cursor < want_end && ended {
        // The stream ended before the requested end (a declared total that lied, or none declared).
        return Err(ReelError::Truncated { delivered });
    }
    let complete = delivery_complete(req, delivered)?;
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
            end_pts: tick_span(cursor, tps)?,
            units: delivered,
        },
        disposition: delivery_disposition(aligned, complete),
        source: SourceKind::Original,
        proxy_note: ProxyNote::NotRequested,
        original_id: req.grant.content_id.clone(),
        proxy_id: None,
        codec: ids.codec,
        decoder_id: ids.decoder_id,
        decoder_version: ids.decoder_version,
        chunks,
        bytes_read: source.bytes_read(),
        complete,
        ticks_exact: true,
        video: None,
    })
}

/// Decode a whole stream into memory, bounded by `max_frames` presented frames (V4-02 aggregate budget:
/// the running total is checked before each block is kept).
pub(crate) fn decode_all(
    source: &mut dyn FrameSource,
    shape: &StreamShape,
    max_frames: u64,
    cancel: &CancellationToken,
) -> Result<Vec<f32>, ReelError> {
    let channels = usize::from(shape.channels);
    if let Some(total) = shape.total
        && total > max_frames
    {
        return Err(ReelError::LimitExceeded {
            what: "audio-frame-budget",
        });
    }
    let mut out: Vec<f32> = Vec::new();
    let mut block = Vec::new();
    let mut decoded: u64 = 0;
    let end = shape.total.and_then(|t| t.checked_add(shape.skip));
    loop {
        check_cancel(cancel)?;
        block.clear();
        let Some(frames) = source.next_block(&mut block, cancel)? else {
            break;
        };
        if block.len() != frames.saturating_mul(channels) {
            return Err(ReelError::Corrupt {
                what: "audio-block-shape",
            });
        }
        let block_start = decoded;
        decoded = decoded
            .checked_add(frames as u64)
            .ok_or(ReelError::LimitExceeded {
                what: "sample-range",
            })?;
        let lo = shape.skip.saturating_sub(block_start).min(frames as u64);
        let hi = end.map_or(frames as u64, |e| {
            e.saturating_sub(block_start).min(frames as u64)
        });
        if hi <= lo {
            if end.is_some_and(|e| decoded >= e) {
                break;
            }
            continue;
        }
        let kept = out.len().checked_div(channels).unwrap_or(0) as u64;
        if kept.saturating_add(hi.saturating_sub(lo)) > max_frames {
            return Err(ReelError::LimitExceeded {
                what: "audio-frame-budget",
            });
        }
        let (a, b) = (
            usize::try_from(lo).unwrap_or(0).saturating_mul(channels),
            usize::try_from(hi).unwrap_or(0).saturating_mul(channels),
        );
        out.try_reserve(b.saturating_sub(a))
            .map_err(|_| ReelError::LimitExceeded {
                what: "audio-alloc",
            })?;
        out.extend_from_slice(block.get(a..b).unwrap_or(&[]));
        if end.is_some_and(|e| decoded >= e) {
            break;
        }
    }
    Ok(out)
}
