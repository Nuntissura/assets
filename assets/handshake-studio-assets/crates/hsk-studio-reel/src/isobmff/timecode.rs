//! QuickTime `tmcd` timecode tracks: sample-entry fields and the raw start sample.
//!
//! Reel exposes RAW fields only. Rendering a label (`HH:MM:SS:FF` / drop-frame `;`) belongs to the
//! timeline owner (`hsk-studio-pulse`); `hsk-studio-reel` is not allowed to depend on pulse, so the
//! caller combines `TimecodeStart::frame_number` with `TimecodeDescription` there.
use super::*;

/// Timecode sample description (QuickTime File Format, 'tmcd'): flags + frame timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimecodeDescription {
    /// Flag 0x0001.
    pub drop_frame: bool,
    /// Flag 0x0002: wraps after 24 hours.
    pub wraps_24h: bool,
    /// Flag 0x0004.
    pub negative_ok: bool,
    /// Flag 0x0008: sample values are tape counter values.
    pub counter: bool,
    pub timescale: u32,
    pub frame_duration: u32,
    /// Nominal frames per second of the timecode format.
    pub frames_per_second: u8,
}

/// `e` is positioned after the common 8 sample-entry bytes.
pub(super) fn parse_tmcd(e: &mut Cur<'_>) -> Result<TimecodeDescription, ReelError> {
    e.skip(4)?; // reserved
    let flags = e.u32()?;
    let timescale = e.u32()?;
    let frame_duration = e.u32()?;
    let frames_per_second = e.take(1)?[0];
    Ok(TimecodeDescription {
        drop_frame: flags & 0x1 != 0,
        wraps_24h: flags & 0x2 != 0,
        negative_ok: flags & 0x4 != 0,
        counter: flags & 0x8 != 0,
        timescale,
        frame_duration,
        frames_per_second,
    })
}

/// The first timecode sample of the `tmcd` track a video/audio track references.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimecodeStart {
    /// The `tmcd` track.
    pub track_id: u32,
    pub description: TimecodeDescription,
    /// The 32-bit big-endian sample as stored.
    pub raw_sample: u32,
    /// `raw_sample` read as a signed frame number (frame counter convention). With the counter flag set
    /// the stored value is a tape counter value (see `description.counter`); not interpreted here.
    pub frame_number: i32,
    /// Presentation time of that sample, in ticks.
    pub pts: i64,
}

/// Read the start timecode for `track_id` through its `tref/tmcd` link. `Ok(None)` when the track has no
/// timecode reference or the referenced track holds no sample. A track the reader refuses
/// (`TrackIndex::unsupported`) is the same typed `Unsupported` it is everywhere else, for the named track
/// and for the `tmcd` track it references: a refused timecode track is not "no timecode" (test
/// `refused_track_is_unsupported_for_every_use`).
pub fn read_timecode_start(
    src: &mut dyn ReadSeek,
    index: &MediaIndex,
    track_id: u32,
    limits: &Limits,
) -> Result<Option<TimecodeStart>, ReelError> {
    let Some(track) = index.track(track_id) else {
        return Err(ReelError::Unsupported {
            what: "track-not-found",
        });
    };
    track.require_supported()?;
    let Some(tc_id) = track.timecode_ref else {
        return Ok(None);
    };
    let tc = index
        .track(tc_id)
        .filter(|t| t.kind == TrackKind::Timecode)
        .ok_or(ReelError::Corrupt {
            what: "tref-tmcd-target",
        })?;
    tc.require_supported()?;
    let (Some(description), Some(first)) = (tc.timecode, tc.presented.first()) else {
        return Ok(None);
    };
    let sample = tc.samples[first.sample as usize];
    if sample.size < 4 {
        return Err(ReelError::Corrupt {
            what: "tmcd-sample-size",
        });
    }
    let mut buf = Vec::new();
    read_sample(src, &SampleEntry { size: 4, ..sample }, &mut buf, limits, 0)?;
    let raw_sample = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
    Ok(Some(TimecodeStart {
        track_id: tc_id,
        description,
        raw_sample,
        frame_number: raw_sample as i32,
        pts: first.pts,
    }))
}
