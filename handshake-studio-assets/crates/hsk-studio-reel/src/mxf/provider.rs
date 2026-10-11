//! `MxfProvider`: the MediaProvider over the MXF index. Essence is opaque: `OutputMode::Packets` delivers
//! the essence element of one track per edit unit (decode order, pre-roll from the nearest earlier random
//! access point), `OutputMode::Decoded` is `CodecUnavailable` naming the essence coding.
//!
//! Not part of `Registry::standard()` (that set is pinned at four providers by the foundation tests):
//! `registry.register(Box::new(MxfProvider))` adds it, after which `Registry::detect` sniffs the header
//! partition label.
use super::{
    DECODER_VERSION, MXF_PROVIDER_ID, MxfIndex, MxfPackageKind, MxfTrack, MxfTrackKind,
    MxfWrapping, build_mxf_index, ticks_to_unit, units_to_ticks,
};
use crate::error::{ReelError, check_cancel};
use crate::model::*;
use hsk_studio_accord::{CancellationToken, Ticks};
use std::io::SeekFrom;

#[derive(Debug, Clone, Copy)]
pub struct MxfProvider;

fn hex_label(label: &[u8; 16]) -> String {
    let mut out = String::with_capacity(47);
    for (i, b) in label.iter().enumerate() {
        if i > 0 {
            out.push('.');
        }
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Name of what a track's essence is coded as: the picture / sound coding label (ST 400), else the
/// essence container label, else the track kind. It is not a decoded format.
fn coding_label(track: &MxfTrack) -> String {
    let descriptor = track.descriptor.as_ref();
    let label = descriptor
        .and_then(|d| d.picture_essence_coding.or(d.sound_essence_coding))
        .or_else(|| descriptor.and_then(|d| d.essence_container));
    match label {
        Some(label) => format!("mxf:{}", hex_label(&label)),
        None => format!("mxf:{:?}", track.kind).to_lowercase(),
    }
}

fn select_track(index: &MxfIndex, wanted: Option<u32>) -> Result<&MxfTrack, ReelError> {
    match wanted {
        Some(id) => index.essence_track(id).ok_or(ReelError::Unsupported {
            what: "track-not-found",
        }),
        None => index.default_track().ok_or(ReelError::Unsupported {
            what: "no-audio-or-video-track",
        }),
    }
}

impl MxfProvider {
    /// Packet delivery over a prebuilt index. The grant is verified against the live source and the
    /// index's source length before any payload read; `bytes_read` counts payload only.
    pub fn decode_packets(
        &self,
        src: &mut dyn ReadSeek,
        index: &MxfIndex,
        req: &SegmentRequest,
        limits: &Limits,
        cancel: &CancellationToken,
        sink: &mut dyn ChunkSink,
    ) -> Result<SegmentReceipt, ReelError> {
        check_cancel(cancel)?;
        if req.codec != MXF_PROVIDER_ID {
            return Err(ReelError::CodecUnavailable {
                codec: req.codec.clone(),
            });
        }
        if req.count == 0 {
            return Err(ReelError::Unsupported { what: "zero-count" });
        }
        let len = verify_grant(src, &req.grant, req.expected_revision)?;
        if len != index.source_len {
            return Err(ReelError::StaleGrant);
        }
        let track = select_track(index, req.track)?;
        if req.output == OutputMode::Decoded {
            // Essence stays opaque: naming the coding is the typed answer.
            return Err(ReelError::CodecUnavailable {
                codec: coding_label(track),
            });
        }
        let index_sid = track.index_sid.ok_or(ReelError::Unsupported {
            what: "mxf-track-without-index",
        })?;
        let table = index.index_table(index_sid).ok_or(ReelError::Unsupported {
            what: "mxf-index-table-not-found",
        })?;
        let rate = table.edit_rate;
        let (start, aligned) = ticks_to_unit(req.start_pts.value(), rate)?;
        let end = start
            .checked_add(req.count)
            .ok_or(ReelError::LimitExceeded { what: "tick-range" })?;
        let total = table
            .indexed_units()
            .or_else(|| track.duration.and_then(|d| u64::try_from(d).ok()))
            .unwrap_or(0);
        if end > total {
            let (first, _) = units_to_ticks(0, rate)?;
            let (last, _) = units_to_ticks(total, rate)?;
            return Err(ReelError::OutOfRange {
                requested: req.start_pts,
                first: i64::try_from(first).unwrap_or(i64::MAX),
                end: i64::try_from(last).unwrap_or(i64::MAX),
            });
        }
        // Export is exact or it fails, before any essence element is read: the start must be a whole
        // edit unit.
        require_exact_start(req, aligned, || {
            Ok(Ticks::new(units_to_ticks(start, rate)?.0))
        })?;
        let unit_ticks = units_to_ticks(1, rate)?.0;
        let constant = table.segments.iter().any(|s| s.edit_unit_byte_count != 0);
        let wrapping = index.wrapping_of(table);
        let mut buf = Vec::new();
        let (mut chunks, mut payload, mut delivered) = (0u32, 0u64, 0u64);
        let mut ticks_exact = true;

        // Clip-wrapped constant-size essence (OP-Atom): the requested units are one contiguous byte
        // range of the clip element, delivered in slices of whole edit units.
        if constant && wrapping == MxfWrapping::Clip {
            let first = index.locate(index_sid, start)?;
            let bytes_per_unit =
                u64::from(first.edit_unit_byte_count.ok_or(ReelError::Unsupported {
                    what: "mxf-clip-wrapped-variable-size",
                })?);
            let max_units = (limits.max_chunk_bytes as u64)
                .checked_div(bytes_per_unit.max(1))
                .unwrap_or(0);
            if max_units == 0 {
                return Err(ReelError::LimitExceeded {
                    what: "chunk-smaller-than-sample",
                });
            }
            let mut position = start;
            while position < end {
                check_cancel(cancel)?;
                let n = end.saturating_sub(position).min(max_units);
                let unit = index.locate(index_sid, position)?;
                let want = n.saturating_mul(bytes_per_unit);
                let wanted_end =
                    unit.file_offset
                        .checked_add(want)
                        .ok_or(ReelError::LimitExceeded {
                            what: "byte-offset",
                        })?;
                if wanted_end > index.clip_end(&unit)? {
                    return Err(ReelError::Truncated { delivered });
                }
                let take = want;
                read_into(src, unit.file_offset, take, &mut buf, delivered)?;
                let (pts, exact) = units_to_ticks(position, rate)?;
                ticks_exact &= exact;
                sink.accept(Chunk {
                    kind: ChunkKind::Packet,
                    pts: i64::try_from(pts)
                        .map_err(|_| ReelError::LimitExceeded { what: "tick-range" })?,
                    duration: Ticks::new(n.saturating_mul(unit_ticks)),
                    index: position,
                    units: n,
                    sync: true,
                    preroll: false,
                    trim_start: 0,
                    bytes: &buf,
                })?;
                chunks = chunks.saturating_add(1);
                payload = payload.saturating_add(take);
                delivered = delivered.saturating_add(n);
                position = position.saturating_add(n);
            }
        } else {
            // Decode order from the nearest earlier random access point to the last coded position of
            // the requested display positions.
            if req.count > limits.max_samples as u64 {
                return Err(ReelError::LimitExceeded {
                    what: "sample-count",
                });
            }
            // Only picture essence is temporally reordered (11.1.6: sound and data elements ignore the
            // temporal offset), so only picture tracks of variable-size indexes take the coded path.
            let reordered = !constant && track.kind == MxfTrackKind::Picture;
            let (mut lo, mut hi) = (u64::MAX, 0u64);
            for d in start..end {
                check_cancel(cancel)?;
                let coded = if reordered {
                    index.locate(index_sid, d)?.coded_position
                } else {
                    d
                };
                lo = lo.min(coded);
                hi = hi.max(coded);
            }
            let mut from = lo;
            if reordered {
                while from > 0 && !index.locate_coded(index_sid, from)?.random_access() {
                    from = from.saturating_sub(1);
                }
            }
            for coded in from..=hi {
                check_cancel(cancel)?;
                let unit = index.locate_coded(index_sid, coded)?;
                let display = if reordered {
                    index.display_position_of(index_sid, coded)?
                } else {
                    coded
                };
                let element = index.edit_unit_element(src, &unit, track.track_number, limits)?;
                read_into(
                    src,
                    element.value_offset,
                    element.value_len,
                    &mut buf,
                    delivered,
                )?;
                let (pts, exact) = units_to_ticks(display, rate)?;
                ticks_exact &= exact;
                let requested = display >= start && display < end;
                sink.accept(Chunk {
                    kind: ChunkKind::Packet,
                    pts: i64::try_from(pts)
                        .map_err(|_| ReelError::LimitExceeded { what: "tick-range" })?,
                    duration: Ticks::new(unit_ticks),
                    index: coded,
                    units: 1,
                    sync: !reordered || unit.random_access(),
                    preroll: !requested,
                    trim_start: 0,
                    bytes: &buf,
                })?;
                chunks = chunks.saturating_add(1);
                payload = payload.saturating_add(element.value_len);
                if requested {
                    delivered = delivered.saturating_add(1);
                }
            }
        }
        let (start_ticks, start_exact) = units_to_ticks(start, rate)?;
        let (end_ticks, end_exact) = units_to_ticks(end, rate)?;
        let span = end_ticks.saturating_sub(start_ticks);
        Ok(SegmentReceipt {
            requested: Span {
                start_pts: req.start_pts,
                end_pts: Ticks::new(req.start_pts.value().saturating_add(span)),
                units: req.count,
            },
            actual: Span {
                start_pts: Ticks::new(start_ticks),
                end_pts: Ticks::new(end_ticks),
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
            codec: coding_label(track),
            decoder_id: "mxf-demux",
            decoder_version: DECODER_VERSION,
            chunks,
            bytes_read: payload,
            complete: delivered == req.count,
            ticks_exact: ticks_exact && start_exact && end_exact,
            video: None,
        })
    }
}

/// Read `len` bytes at `pos` into `buf` (a short source is `Truncated`).
fn read_into(
    src: &mut dyn ReadSeek,
    pos: u64,
    len: u64,
    buf: &mut Vec<u8>,
    delivered: u64,
) -> Result<(), ReelError> {
    let len = usize::try_from(len).map_err(|_| ReelError::LimitExceeded {
        what: "sample-bytes",
    })?;
    buf.clear();
    buf.resize(len, 0);
    src.seek(SeekFrom::Start(pos))?;
    read_exact_or_truncated(src, buf, delivered)
}

impl MediaProvider for MxfProvider {
    fn descriptor(&self) -> CodecDescriptor {
        CodecDescriptor {
            id: MXF_PROVIDER_ID,
            kind: MediaKind::Container,
            container: "mxf-op1a-opatom",
            bit_depths: &[],
            chroma: None,
            transfer: None,
            decoder_version: DECODER_VERSION,
            can_decode: false,
            can_encode: false,
        }
    }

    fn sniff(&self, head: &[u8]) -> bool {
        head.len() >= 16
            && head[..4] == [0x06, 0x0E, 0x2B, 0x34]
            && head[4..7] == [0x02, 0x05, 0x01]
            && head[8..13] == [0x0D, 0x01, 0x02, 0x01, 0x01]
            && head[13] == 0x02
    }

    fn probe(
        &self,
        src: &mut dyn ReadSeek,
        limits: &Limits,
        cancel: &CancellationToken,
    ) -> Result<StreamLayout, ReelError> {
        let len = src.seek(SeekFrom::End(0))?;
        let index = build_mxf_index(src, len, limits, cancel)?;
        let track = index.default_track().ok_or(ReelError::Unsupported {
            what: "no-audio-or-video-track",
        })?;
        let table = track
            .index_sid
            .and_then(|sid| index.index_table(sid))
            .ok_or(ReelError::Unsupported {
                what: "mxf-track-without-index",
            })?;
        let units = table
            .indexed_units()
            .or_else(|| track.duration.and_then(|d| u64::try_from(d).ok()))
            .unwrap_or(0);
        let descriptor = track.descriptor.as_ref();
        let essence_tracks = index
            .tracks
            .iter()
            .filter(|t| t.package_kind == MxfPackageKind::Source && t.track_number != 0)
            .count();
        Ok(StreamLayout {
            kind: if track.kind == MxfTrackKind::Picture {
                MediaKind::Video
            } else {
                MediaKind::Audio
            },
            codec: coding_label(track),
            width: descriptor.and_then(|d| d.sampled_width).unwrap_or(0),
            height: descriptor.and_then(|d| d.sampled_height).unwrap_or(0),
            ticks_per_unit: units_to_ticks(1, table.edit_rate)?.0,
            units,
            channels: descriptor
                .and_then(|d| d.channel_count)
                .and_then(|c| u16::try_from(c).ok())
                .unwrap_or(0),
            tracks: u32::try_from(essence_tracks).unwrap_or(u32::MAX),
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
        if req.codec != MXF_PROVIDER_ID {
            return Err(ReelError::CodecUnavailable {
                codec: req.codec.clone(),
            });
        }
        if req.count == 0 {
            return Err(ReelError::Unsupported { what: "zero-count" });
        }
        let file_len = verify_grant(src, &req.grant, req.expected_revision)?;
        let index = build_mxf_index(src, file_len, limits, cancel)?;
        let mut receipt = self.decode_packets(src, &index, req, limits, cancel, sink)?;
        receipt.bytes_read = receipt.bytes_read.saturating_add(index.bytes_read);
        Ok(receipt)
    }
}
