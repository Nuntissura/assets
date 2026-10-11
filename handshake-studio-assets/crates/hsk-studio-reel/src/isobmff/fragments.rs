//! Fragmented MP4 / CMAF index: `mvex/trex`, `moof/mfhd/traf/tfhd/tfdt/trun`, `sidx`.
//!
//! Fragments are scanned by top-level box header (seeking over `mdat`); only `moof` and `sidx` payloads
//! are read, within the same `max_probe_bytes` budget as `moov`. Every sample goes through the shared
//! sample budget, so a hostile `trun` count fails before any allocation. Layouts follow ISO/IEC
//! 14496-12: tfhd flags 0x1 base-data-offset, 0x2 sample-description-index, 0x8/0x10/0x20 defaults,
//! 0x10000 duration-is-empty, 0x20000 default-base-is-moof; trun flags 0x1 data-offset, 0x4
//! first-sample-flags, 0x100 duration, 0x200 size, 0x400 flags, 0x800 composition offset; sample flag
//! bit 16 (0x10000) = non-sync.
use super::*;

const MAX_FRAGMENT_BOXES: u32 = 1 << 20;
const NON_SYNC_FLAG: u32 = 0x0001_0000;

/// One `sidx` reference with its absolute file offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentIndexEntry {
    pub offset: u64,
    pub size: u32,
    pub duration: u32,
    pub starts_with_sap: bool,
    /// Reference type 1: points at another `sidx`, not at media.
    pub is_index: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentIndex {
    pub reference_id: u32,
    pub timescale: u32,
    pub earliest_pts: u64,
    pub entries: Vec<SegmentIndexEntry>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Trex {
    track_id: u32,
    duration: u32,
    size: u32,
    flags: u32,
}

pub(super) struct Scan {
    pub fragment_offsets: Vec<u64>,
    pub segment_indexes: Vec<SegmentIndex>,
    pub truncated_tail: bool,
}

struct TrackState {
    next_dts: i64,
    appended: bool,
}

pub(super) fn parse_trex_list(mvex: &[u8]) -> Result<Vec<Trex>, ReelError> {
    let mut out = Vec::new();
    for item in boxes(mvex) {
        let (kind, payload) = item?;
        if &kind == b"trex" {
            let (_, _, mut c) = full(payload)?;
            let track_id = c.u32()?;
            c.skip(4)?; // default_sample_description_index
            out.push(Trex {
                track_id,
                duration: c.u32()?,
                size: c.u32()?,
                flags: c.u32()?,
            });
        }
    }
    Ok(out)
}

/// Scan top-level boxes from `pos` (just after `moov`), appending every fragment sample to its track.
#[allow(clippy::too_many_arguments)]
pub(super) fn scan(
    src: &mut dyn ReadSeek,
    file_len: u64,
    mut pos: u64,
    limits: &Limits,
    cancel: &CancellationToken,
    bytes_read: &mut u64,
    tracks: &mut [TrackIndex],
    trex: &[Trex],
    budget: &mut Budget,
) -> Result<Scan, ReelError> {
    let mut state: Vec<TrackState> = tracks
        .iter()
        .map(|t| TrackState {
            next_dts: t
                .samples
                .last()
                .map_or(0, |s| s.dts.saturating_add(i64::from(s.duration))),
            appended: false,
        })
        .collect();
    let mut result = Scan {
        fragment_offsets: Vec::new(),
        segment_indexes: Vec::new(),
        truncated_tail: false,
    };
    for _ in 0..MAX_FRAGMENT_BOXES {
        check_cancel(cancel)?;
        let remaining = file_len.saturating_sub(pos);
        if remaining < 8 {
            break;
        }
        src.seek(SeekFrom::Start(pos))?;
        let mut hdr = [0u8; 8];
        read_exact_structural(src, &mut hdr, "box-header")?;
        *bytes_read += 8;
        let size32 = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
        let kind = [hdr[4], hdr[5], hdr[6], hdr[7]];
        let (header, size) = match size32 {
            0 => (8u64, remaining),
            1 => {
                let mut big = [0u8; 8];
                read_exact_structural(src, &mut big, "box-largesize")?;
                *bytes_read += 8;
                (16, u64::from_be_bytes(big))
            }
            n => (8, u64::from(n)),
        };
        if size < header {
            return Err(ReelError::Corrupt { what: "box-size" });
        }
        let truncated = size > remaining;
        match &kind {
            b"moof" | b"sidx" => {
                if truncated {
                    result.truncated_tail = true;
                    break;
                }
                let payload_len = size - header;
                if payload_len > limits.max_probe_bytes.saturating_sub(*bytes_read) {
                    return Err(ReelError::LimitExceeded {
                        what: "fragment-index-bytes",
                    });
                }
                src.seek(SeekFrom::Start(pos + header))?;
                let mut buf = vec![0u8; payload_len as usize];
                read_exact_structural(src, &mut buf, "fragment-truncated")?;
                *bytes_read += payload_len;
                if &kind == b"moof" {
                    parse_moof(&buf, pos, tracks, &mut state, trex, budget)?;
                    result.fragment_offsets.push(pos);
                } else {
                    result.segment_indexes.push(parse_sidx(&buf, pos + size)?);
                }
            }
            b"moov" => {
                return Err(ReelError::Corrupt {
                    what: "second-moov",
                });
            }
            _ => {
                if truncated {
                    result.truncated_tail = true;
                    break;
                }
            }
        }
        pos = match pos.checked_add(size) {
            Some(next) => next,
            None => break,
        };
    }
    for (track, st) in tracks.iter_mut().zip(&state) {
        if st.appended {
            track.sync_points = track
                .samples
                .iter()
                .enumerate()
                .filter(|(_, s)| s.sync)
                .map(|(i, _)| i as u32)
                .collect();
            track.all_sync = track.sync_points.len() == track.samples.len();
            if track.all_sync {
                track.sync_points.clear();
            }
        }
    }
    Ok(result)
}

fn parse_moof(
    payload: &[u8],
    moof_start: u64,
    tracks: &mut [TrackIndex],
    state: &mut [TrackState],
    trex: &[Trex],
    budget: &mut Budget,
) -> Result<(), ReelError> {
    let (mut prev_end, mut first_traf) = (None::<u64>, true);
    for item in boxes(payload) {
        let (kind, traf) = item?;
        if &kind == b"traf" {
            let end = parse_traf(
                traf, moof_start, first_traf, prev_end, tracks, state, trex, budget,
            )?;
            prev_end = end.or(prev_end);
            first_traf = false;
        }
    }
    Ok(())
}

/// Returns the end offset of this track fragment's data (the base for a following `traf` when neither
/// an explicit base nor default-base-is-moof applies).
#[allow(clippy::too_many_arguments)]
fn parse_traf(
    traf: &[u8],
    moof_start: u64,
    first_traf: bool,
    prev_end: Option<u64>,
    tracks: &mut [TrackIndex],
    state: &mut [TrackState],
    trex: &[Trex],
    budget: &mut Budget,
) -> Result<Option<u64>, ReelError> {
    let corrupt = |what| ReelError::Corrupt { what };
    let tfhd = child(traf, b"tfhd")?.ok_or(corrupt("tfhd-missing"))?;
    let (_, flags, mut c) = full(tfhd)?;
    let track_id = c.u32()?;
    let explicit_base = if flags & 0x1 != 0 {
        Some(c.u64()?)
    } else {
        None
    };
    if flags & 0x2 != 0 && c.u32()? != 1 {
        return Err(ReelError::Unsupported {
            what: "multiple-sample-descriptions",
        });
    }
    let mut optional = |bit: u32| -> Result<Option<u32>, ReelError> {
        if flags & bit != 0 {
            c.u32().map(Some)
        } else {
            Ok(None)
        }
    };
    let default_duration = optional(0x08)?;
    let default_size = optional(0x10)?;
    let default_flags = optional(0x20)?;
    let ti = tracks
        .iter()
        .position(|t| t.track_id == track_id)
        .ok_or(corrupt("traf-unknown-track"))?;
    let defaults = trex
        .iter()
        .find(|t| t.track_id == track_id)
        .copied()
        .unwrap_or(Trex {
            track_id,
            duration: 0,
            size: 0,
            flags: 0,
        });
    if flags & 0x1_0000 != 0 {
        return Ok(None); // duration-is-empty: no samples in this fragment
    }
    let base = explicit_base.unwrap_or(if flags & 0x2_0000 != 0 || first_traf {
        moof_start
    } else {
        prev_end.unwrap_or(moof_start)
    });
    let mut dts = match child(traf, b"tfdt")? {
        Some(payload) => {
            let (version, _, mut t) = full(payload)?;
            let time = if version == 1 {
                t.u64()?
            } else {
                u64::from(t.u32()?)
            };
            i64::try_from(time).map_err(|_| ReelError::LimitExceeded { what: "time-range" })?
        }
        None => state[ti].next_dts,
    };
    let (mut next_run_offset, mut end) = (base, None);
    for item in boxes(traf) {
        let (kind, payload) = item?;
        if &kind != b"trun" {
            continue;
        }
        let (_, tflags, mut c) = full(payload)?;
        let count = c.u32()? as usize;
        let data_offset = if tflags & 0x1 != 0 {
            Some(c.i32()?)
        } else {
            None
        };
        let first_flags = if tflags & 0x4 != 0 {
            Some(c.u32()?)
        } else {
            None
        };
        let per_sample = 4 * [0x100u32, 0x200, 0x400, 0x800]
            .iter()
            .filter(|bit| tflags & **bit != 0)
            .count();
        if per_sample > 0
            && count
                .checked_mul(per_sample)
                .is_none_or(|bytes| bytes > c.remaining())
        {
            return Err(corrupt("trun-exceeds-box"));
        }
        if count > budget.samples_left {
            return Err(ReelError::LimitExceeded {
                what: "sample-count",
            });
        }
        budget.samples_left -= count;
        let mut offset = match data_offset {
            Some(delta) => base
                .checked_add_signed(i64::from(delta))
                .ok_or(corrupt("trun-data-offset"))?,
            None => next_run_offset,
        };
        tracks[ti].samples.reserve(count);
        for i in 0..count {
            let duration = if tflags & 0x100 != 0 {
                c.u32()?
            } else {
                default_duration.unwrap_or(defaults.duration)
            };
            let size = if tflags & 0x200 != 0 {
                c.u32()?
            } else {
                default_size.unwrap_or(defaults.size)
            };
            let per_flags = if tflags & 0x400 != 0 {
                Some(c.u32()?)
            } else {
                None
            };
            let cto = if tflags & 0x800 != 0 { c.i32()? } else { 0 };
            let sample_flags = if i == 0 {
                first_flags.or(per_flags)
            } else {
                per_flags
            }
            .unwrap_or(default_flags.unwrap_or(defaults.flags));
            tracks[ti].samples.push(SampleEntry {
                dts,
                pts: dts
                    .checked_add(i64::from(cto))
                    .ok_or(ReelError::LimitExceeded { what: "time-range" })?,
                duration,
                offset,
                size,
                sync: sample_flags & NON_SYNC_FLAG == 0,
            });
            dts = dts
                .checked_add(i64::from(duration))
                .ok_or(ReelError::LimitExceeded { what: "time-range" })?;
            offset = offset
                .checked_add(u64::from(size))
                .ok_or(corrupt("chunk-offset-overflow"))?;
        }
        next_run_offset = offset;
        end = Some(offset);
    }
    state[ti].next_dts = dts;
    state[ti].appended = true;
    Ok(end)
}

/// `anchor` is the absolute offset of the first byte after the `sidx` box.
fn parse_sidx(payload: &[u8], anchor: u64) -> Result<SegmentIndex, ReelError> {
    let (version, _, mut c) = full(payload)?;
    let reference_id = c.u32()?;
    let timescale = c.u32()?;
    let (earliest_pts, first_offset) = if version == 0 {
        (u64::from(c.u32()?), u64::from(c.u32()?))
    } else {
        (c.u64()?, c.u64()?)
    };
    c.skip(2)?; // reserved
    let count = usize::from(c.u16()?);
    if count * 12 > c.remaining() {
        return Err(ReelError::Corrupt {
            what: "sidx-exceeds-box",
        });
    }
    let mut offset = anchor.checked_add(first_offset).ok_or(ReelError::Corrupt {
        what: "sidx-offset",
    })?;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let word = c.u32()?;
        let duration = c.u32()?;
        let sap = c.u32()?;
        let size = word & 0x7FFF_FFFF;
        entries.push(SegmentIndexEntry {
            offset,
            size,
            duration,
            starts_with_sap: sap >> 31 == 1,
            is_index: word >> 31 == 1,
        });
        offset = offset
            .checked_add(u64::from(size))
            .ok_or(ReelError::Corrupt {
                what: "sidx-offset",
            })?;
    }
    Ok(SegmentIndex {
        reference_id,
        timescale,
        earliest_pts,
        entries,
    })
}
