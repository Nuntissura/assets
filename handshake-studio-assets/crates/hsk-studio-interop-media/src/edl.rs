//! CMX3600 EDL import and deterministic export (cuts-only subset).
//!
//! Imported: TITLE, FCM, cut events on V / A / A2.. / AA / B / AA/V tracks, `M2` speed changes
//! (exact rational), `* FROM/TO CLIP NAME:` names. Reported as typed losses, never silent:
//! dissolves, wipes and keys (kept as cuts), freeze frames, unknown tracks, unknown reels,
//! stereo-pair collapse, M2/span disagreement, other directives. Input bytes are never modified
//! and a failure returns no partial proposal.
use crate::error::{ErrorKind, InteropError, Result};
use crate::model::{
    ClipProposal, Disposition, Lane, LaneKind, LossNote, SourceFormat, TimelineProposal,
};
use crate::time::{
    fps_fraction, frames_to_ticks, frames_to_timecode, reduce, ticks_to_frames_exact,
    timecode_to_frames,
};
use hsk_studio_accord::{CancellationToken, FrameRate, Ticks};
use hsk_studio_folio::Speed;
use std::collections::BTreeMap;

pub struct ImportRequest<'a> {
    pub frame_rate: FrameRate,
    /// Drop-frame when the file has no `FCM:` line.
    pub assume_drop: bool,
    /// Frames subtracted from every record timecode (0 keeps the file's absolute timecode).
    pub record_origin_frames: u64,
    /// When given, a reel outside the list is imported with a `Missing` loss.
    pub known_reels: Option<&'a [&'a str]>,
}

impl ImportRequest<'_> {
    pub fn new(frame_rate: FrameRate) -> Self {
        Self {
            frame_rate,
            assume_drop: false,
            record_origin_frames: 0,
            known_reels: None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_bytes: usize,
    pub max_events: usize,
    pub max_line_len: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 8 * 1024 * 1024,
            max_events: 100_000,
            max_line_len: 1024,
        }
    }
}

struct RawEvent {
    number: u32,
    line: u32,
    reel: String,
    track: String,
    transition: String,
    src_in: u64,
    src_out: u64,
    rec_in: u64,
    rec_out: u64,
    name: Option<String>,
    /// Signed M2 fps numerator and its decimal denominator (`050.0` -> 500/10).
    m2: Option<(i64, u64)>,
}

impl RawEvent {
    fn anchor(&self) -> bool {
        self.transition.starts_with('C')
            && self.src_in == self.src_out
            && self.rec_in == self.rec_out
    }
    fn transition_event(&self) -> bool {
        !self.transition.starts_with('C')
    }
    fn entity(&self) -> String {
        format!("event {:03}", self.number)
    }
}

fn err(kind: ErrorKind, line: u32) -> InteropError {
    InteropError::at(kind, line)
}

fn valid_name(text: &str, max: usize) -> bool {
    !text.is_empty() && text.len() <= max && !text.chars().any(char::is_control)
}

fn decimal(text: &str) -> Option<(i64, u64)> {
    let (sign, body) = match text.as_bytes().first()? {
        b'-' => (-1i64, &text[1..]),
        b'+' => (1, &text[1..]),
        _ => (1, text),
    };
    let (whole, fraction) = body.split_once('.').unwrap_or((body, ""));
    if whole.is_empty() || whole.len() > 6 || fraction.len() > 3 {
        return None;
    }
    if !whole
        .bytes()
        .chain(fraction.bytes())
        .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let digits = format!("{whole}{fraction}");
    Some((
        sign * digits.parse::<i64>().ok()?,
        10u64.pow(fraction.len() as u32),
    ))
}

fn parse_event(
    tokens: &[&str],
    line: u32,
    rate: FrameRate,
    drop: bool,
    name_cap: usize,
) -> Result<RawEvent> {
    let number = tokens[0]
        .parse::<u32>()
        .ok()
        .filter(|n| *n > 0 && tokens[0].len() <= 6)
        .ok_or(err(ErrorKind::InvalidEventNumber, line))?;
    if tokens.len() < 8 || !valid_name(tokens[1], name_cap) {
        return Err(err(ErrorKind::InvalidLine, line));
    }
    let mut rest = &tokens[4..];
    if !rest[0].contains([':', ';']) {
        // Transition duration in frames (dissolves, wipes); validated, not mapped.
        if !rest[0].bytes().all(|b| b.is_ascii_digit()) {
            return Err(err(ErrorKind::InvalidLine, line));
        }
        rest = &rest[1..];
    }
    if rest.len() != 4 {
        return Err(err(ErrorKind::InvalidLine, line));
    }
    let tc = |i: usize| timecode_to_frames(rest[i], rate, drop).map_err(|e| err(e.kind, line));
    Ok(RawEvent {
        number,
        line,
        reel: tokens[1].to_owned(),
        track: tokens[2].to_ascii_uppercase(),
        transition: tokens[3].to_ascii_uppercase(),
        src_in: tc(0)?,
        src_out: tc(1)?,
        rec_in: tc(2)?,
        rec_out: tc(3)?,
        name: None,
        m2: None,
    })
}

enum Mapped {
    Lanes {
        lanes: Vec<Lane>,
        linked: bool,
        pair_collapsed: bool,
    },
    Skip(&'static str),
}

fn map_track(token: &str) -> Mapped {
    let video = Lane {
        kind: LaneKind::Video,
        index: 0,
    };
    let audio = |index| Lane {
        kind: LaneKind::Audio,
        index,
    };
    match token {
        "V" => Mapped::Lanes {
            lanes: vec![video],
            linked: false,
            pair_collapsed: false,
        },
        "A" => Mapped::Lanes {
            lanes: vec![audio(0)],
            linked: false,
            pair_collapsed: false,
        },
        "B" => Mapped::Lanes {
            lanes: vec![video, audio(0)],
            linked: true,
            pair_collapsed: false,
        },
        "AA" => Mapped::Lanes {
            lanes: vec![audio(0)],
            linked: false,
            pair_collapsed: true,
        },
        "AA/V" => Mapped::Lanes {
            lanes: vec![video, audio(0)],
            linked: true,
            pair_collapsed: true,
        },
        "NONE" => Mapped::Skip("no_track"),
        other => match other.strip_prefix('A').map(|n| (n, n.parse::<u32>())) {
            Some((digits, Ok(n)))
                if (2..=64).contains(&n) && digits.bytes().all(|b| b.is_ascii_digit()) =>
            {
                Mapped::Lanes {
                    lanes: vec![audio(n - 1)],
                    linked: false,
                    pair_collapsed: false,
                }
            }
            _ => Mapped::Skip("track_unsupported"),
        },
    }
}

/// Speed from the actual source/record lengths (exact by construction); M2 only cross-checks it.
/// Callers have already rejected empty ranges and freeze frames.
fn derive_speed(
    ev: &RawEvent,
    fps: (u64, u64),
    losses: &mut Vec<LossNote>,
) -> Result<(Speed, Disposition)> {
    let (span, duration) = (ev.src_out - ev.src_in, ev.rec_out - ev.rec_in);
    let Some((m2_num, m2_den)) = ev.m2 else {
        if span != duration {
            return Err(err(ErrorKind::LengthMismatch, ev.line));
        }
        return Ok((Speed::UNIT, Disposition::Editable));
    };
    let (n, d) = reduce(span, duration).map_err(|_| err(ErrorKind::InvalidSpeed, ev.line))?;
    // M2 ratio = (|m2_num| / m2_den) / (fps_n / fps_d)
    let m2_n = u128::from(m2_num.unsigned_abs()) * u128::from(fps.1);
    let m2_d = u128::from(m2_den) * u128::from(fps.0);
    if u128::from(n) * m2_d != m2_n * u128::from(d) {
        losses.push(LossNote::new(
            ev.entity(),
            "speed",
            Disposition::Approximate,
            "m2_speed_mismatch_spans_used",
        ));
    }
    let magnitude = i64::try_from(n).map_err(|_| err(ErrorKind::InvalidSpeed, ev.line))?;
    let speed = Speed {
        numerator: if m2_num < 0 { -magnitude } else { magnitude },
        denominator: d,
    };
    let disposition = if speed == Speed::UNIT {
        Disposition::Editable
    } else {
        Disposition::Translated
    };
    Ok((speed, disposition))
}

fn transition_loss(ev: &RawEvent) -> Option<LossNote> {
    let code = match ev.transition.chars().next()? {
        'C' => return None,
        'D' => "dissolve_dropped",
        'W' => "wipe_dropped",
        'K' => "key_dropped",
        _ => "transition_dropped",
    };
    Some(LossNote::new(
        ev.entity(),
        "transition",
        Disposition::Unsupported,
        code,
    ))
}

/// Imports a CMX3600 EDL into a neutral proposal.
pub fn import_cmx3600(
    bytes: &[u8],
    request: &ImportRequest,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<TimelineProposal> {
    cancel.check()?;
    if bytes.len() > limits.max_bytes {
        return Err(InteropError::new(ErrorKind::InputLimit));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| InteropError::new(ErrorKind::InvalidUtf8))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rate = request.frame_rate;
    let mut drop = request.assume_drop;
    let mut title = None;
    let mut events: Vec<RawEvent> = Vec::new();
    let mut directives: BTreeMap<String, u32> = BTreeMap::new();
    let mut ignored_comments = 0u32;
    for (index, raw) in text.lines().enumerate() {
        let line = u32::try_from(index + 1).unwrap_or(u32::MAX);
        if raw.len() > limits.max_line_len {
            return Err(err(ErrorKind::LineLimit, line));
        }
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let upper = trimmed.to_ascii_uppercase();
        if let Some(rest) = upper.strip_prefix("TITLE:") {
            let value = trimmed[trimmed.len() - rest.len()..].trim();
            if !value.is_empty() {
                if !valid_name(value, 256) {
                    return Err(err(ErrorKind::InvalidName, line));
                }
                title = Some(value.to_owned());
            }
        } else if let Some(rest) = upper.strip_prefix("FCM:") {
            drop = match rest.trim() {
                "NON-DROP FRAME" => false,
                "DROP FRAME" => true,
                _ => return Err(err(ErrorKind::InvalidLine, line)),
            };
        } else if let Some(comment) = trimmed.strip_prefix('*') {
            let comment = comment.trim();
            let upper_comment = comment.to_ascii_uppercase();
            let named = ["FROM CLIP NAME:", "TO CLIP NAME:"]
                .iter()
                .find(|p| upper_comment.starts_with(**p));
            let count = events.len();
            let target = match named {
                Some(prefix) if count > 0 => {
                    let to = prefix.starts_with("TO");
                    // FROM names the outgoing clip of a transition (the previous event).
                    let index = if events[count - 1].transition_event() && !to {
                        count.checked_sub(2)
                    } else {
                        Some(count - 1)
                    };
                    index.map(|i| (i, prefix.len(), to))
                }
                _ => None,
            };
            match target {
                Some((i, prefix_len, to)) => {
                    let name = comment[prefix_len..].trim();
                    if !valid_name(name, 256) {
                        return Err(err(ErrorKind::InvalidName, line));
                    }
                    if events[i].name.is_none() || to {
                        events[i].name = Some(name.to_owned());
                    } else {
                        ignored_comments += 1;
                    }
                }
                None => ignored_comments += 1,
            }
        } else {
            let tokens: Vec<&str> = trimmed.split_whitespace().collect();
            if tokens[0].bytes().all(|b| b.is_ascii_digit()) {
                if events.len() >= limits.max_events {
                    return Err(err(ErrorKind::EventLimit, line));
                }
                cancel.check()?;
                let event = parse_event(&tokens, line, rate, drop, 128)?;
                if let Some(previous) = events.last() {
                    match event.number.cmp(&previous.number) {
                        std::cmp::Ordering::Less => {
                            return Err(err(ErrorKind::NonMonotonicEvent, line));
                        }
                        std::cmp::Ordering::Equal if !previous.anchor() => {
                            return Err(err(ErrorKind::DuplicateEvent, line));
                        }
                        _ => {}
                    }
                }
                events.push(event);
            } else if tokens[0].eq_ignore_ascii_case("M2") {
                let last = events.last_mut().ok_or(err(ErrorKind::InvalidLine, line))?;
                if tokens.len() != 4 || tokens[1] != last.reel || last.m2.is_some() {
                    return Err(err(ErrorKind::InvalidLine, line));
                }
                timecode_to_frames(tokens[3], rate, drop).map_err(|e| err(e.kind, line))?;
                last.m2 = Some(decimal(tokens[2]).ok_or(err(ErrorKind::InvalidSpeed, line))?);
            } else {
                *directives
                    .entry(tokens[0].to_ascii_uppercase())
                    .or_insert(0) += 1;
            }
        }
    }
    if events.is_empty() {
        return Err(InteropError::new(ErrorKind::NoEvents));
    }
    let fps = fps_fraction(rate);
    let mut losses = Vec::new();
    let mut clips: Vec<ClipProposal> = Vec::new();
    let mut lines: Vec<u32> = Vec::new();
    for ev in &events {
        cancel.check()?;
        if ev.anchor() {
            // Zero-length cut that precedes a transition event: carries no clip.
            losses.push(LossNote::new(
                ev.entity(),
                "event",
                Disposition::Translated,
                "zero_length_cut_dropped",
            ));
            continue;
        }
        if ev.rec_out <= ev.rec_in {
            return Err(err(ErrorKind::InvalidRange, ev.line));
        }
        if matches!(ev.m2, Some((0, _))) {
            // Freeze frame: a zero-length source window is not a valid Folio clip.
            losses.push(LossNote::new(
                ev.entity(),
                "speed",
                Disposition::Unsupported,
                "freeze_frame_dropped",
            ));
            continue;
        }
        if ev.src_out <= ev.src_in {
            return Err(err(ErrorKind::InvalidRange, ev.line));
        }
        let (speed, disposition) = derive_speed(ev, fps, &mut losses)?;
        if ev.rec_in < request.record_origin_frames {
            return Err(err(ErrorKind::BeforeOrigin, ev.line));
        }
        let (lanes, link, collapsed) = match map_track(&ev.track) {
            Mapped::Lanes {
                lanes,
                linked,
                pair_collapsed,
            } => (lanes, linked, pair_collapsed),
            Mapped::Skip(code) => {
                losses.push(LossNote::new(
                    ev.entity(),
                    "track",
                    Disposition::Unsupported,
                    code,
                ));
                continue;
            }
        };
        if let Some(note) = transition_loss(ev) {
            losses.push(note);
        }
        if request
            .known_reels
            .is_some_and(|known| !known.contains(&ev.reel.as_str()))
        {
            losses.push(LossNote::new(
                ev.entity(),
                "source_ref",
                Disposition::Missing,
                "reel_unknown",
            ));
        }
        if collapsed {
            losses.push(LossNote::new(
                ev.entity(),
                "audio_channels",
                Disposition::Approximate,
                "channel_pair_collapsed",
            ));
        }
        let at = |frames: u64| frames_to_ticks(frames, rate).map_err(|e| err(e.kind, ev.line));
        let (source_in, source_out) = (at(ev.src_in)?, at(ev.src_out)?);
        let record_in = at(ev.rec_in - request.record_origin_frames)?;
        let record_out = at(ev.rec_out - request.record_origin_frames)?;
        for lane in lanes {
            let lane_disposition = if collapsed && lane.kind == LaneKind::Audio {
                Disposition::Approximate
            } else {
                disposition
            };
            clips.push(ClipProposal {
                event: ev.number,
                reel: ev.reel.clone(),
                clip_name: ev.name.clone(),
                lane,
                source_in,
                source_out,
                record_in,
                record_out,
                speed,
                disposition: lane_disposition,
                link: link.then_some(ev.number),
            });
            lines.push(ev.line);
        }
    }
    // Clips on one lane must not overlap in record time (touching is allowed).
    let mut by_lane: BTreeMap<Lane, Vec<usize>> = BTreeMap::new();
    for (i, clip) in clips.iter().enumerate() {
        by_lane.entry(clip.lane).or_default().push(i);
    }
    for indexes in by_lane.values_mut() {
        indexes.sort_by_key(|i| (clips[*i].record_in, *i));
        for pair in indexes.windows(2) {
            if clips[pair[1]].record_in < clips[pair[0]].record_out {
                return Err(err(ErrorKind::OverlappingEvents, lines[pair[1]]));
            }
        }
    }
    for directive in directives.keys() {
        losses.push(LossNote::new(
            format!("directive {directive}"),
            "directive",
            Disposition::Unsupported,
            "directive_dropped",
        ));
    }
    Ok(TimelineProposal {
        source_format: SourceFormat::Cmx3600Edl,
        title,
        frame_rate: rate,
        drop_frame: drop,
        record_origin_frames: request.record_origin_frames,
        clips,
        losses,
        source_len: bytes.len() as u64,
        ignored_comment_lines: ignored_comments,
    })
}

fn track_token(lane: Lane) -> Result<String> {
    match (lane.kind, lane.index) {
        (LaneKind::Video, 0) => Ok("V".into()),
        (LaneKind::Audio, 0) => Ok("A".into()),
        (LaneKind::Audio, n) => Ok(format!("A{}", n + 1)),
        (LaneKind::Video, _) => Err(InteropError::new(ErrorKind::TrackUnsupported)),
    }
}

/// M2 fps value as an exact decimal with 1-3 fraction digits, else `speed_not_representable`.
fn m2_value(speed: Speed, fps: (u64, u64)) -> Result<String> {
    let numerator = u128::from(speed.numerator.unsigned_abs()) * u128::from(fps.0);
    let denominator = u128::from(speed.denominator) * u128::from(fps.1);
    for digits in 1..=3u32 {
        let scale = 10u128.pow(digits);
        if (numerator * scale) % denominator == 0 {
            let scaled = numerator * scale / denominator;
            return Ok(format!(
                "{}{:03}.{:0width$}",
                if speed.is_reversed() { "-" } else { "" },
                scaled / scale,
                scaled % scale,
                width = digits as usize
            ));
        }
    }
    Err(InteropError::new(ErrorKind::SpeedNotRepresentable))
}

/// Deterministic CMX3600 text for the round-trip-able subset: cuts on V (index 0) and A/A2..
/// tracks, names, speed. Dissolves, links and the other recorded losses are not re-exported.
pub fn export_cmx3600(proposal: &TimelineProposal) -> Result<String> {
    let rate = proposal.frame_rate;
    let fps = fps_fraction(rate);
    let mut order: Vec<&ClipProposal> = proposal.clips.iter().collect();
    order.sort_by_key(|c| (c.record_in, c.lane));
    let mut out = String::new();
    if let Some(title) = &proposal.title {
        out.push_str(&format!("TITLE: {title}\n"));
    }
    out.push_str(if proposal.drop_frame {
        "FCM: DROP FRAME\n\n"
    } else {
        "FCM: NON-DROP FRAME\n\n"
    });
    let timecode = |ticks: Ticks, origin: u64| -> Result<String> {
        let frames = ticks_to_frames_exact(ticks, rate)?
            .checked_add(origin)
            .ok_or(InteropError::new(ErrorKind::TimecodeOverflow))?;
        frames_to_timecode(frames, rate, proposal.drop_frame)
    };
    for (n, clip) in order.iter().enumerate() {
        let (src_in, src_out) = (timecode(clip.source_in, 0)?, timecode(clip.source_out, 0)?);
        let origin = proposal.record_origin_frames;
        let (rec_in, rec_out) = (
            timecode(clip.record_in, origin)?,
            timecode(clip.record_out, origin)?,
        );
        out.push_str(&format!(
            "{:03}  {:<8} {:<5} C        {src_in} {src_out} {rec_in} {rec_out}\n",
            n + 1,
            clip.reel,
            track_token(clip.lane)?
        ));
        if let Some(name) = &clip.clip_name {
            out.push_str(&format!("* FROM CLIP NAME: {name}\n"));
        }
        if clip.speed != Speed::UNIT {
            out.push_str(&format!(
                "M2   {:<8} {}   {src_in}\n",
                clip.reel,
                m2_value(clip.speed, fps)?
            ));
        }
    }
    Ok(out)
}
