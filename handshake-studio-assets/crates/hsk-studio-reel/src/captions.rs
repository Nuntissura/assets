//! Caption streams in export (lane SMB-PR-SOURCE-MONITOR slice 4; STU-VID-037 "Caption tracks participate
//! in export ([STU-VID-061], facet `captions`)", STU-VID-085 GAP caption formats): timed caption cues become
//! (a) an embedded 3GPP timed-text track (`tx3g`, the stream FFmpeg calls `mov_text`; 3GPP TS 26.245) in the
//! MP4 / MOV the export writes, and (b) a WebVTT sidecar file (W3C WebVTT) published beside it.
//!
//! Timing: cue boundaries are Pulse ticks (254016000000 per second); the text track's media timescale is
//! 1000 (milliseconds, as FFmpeg's `mov_text` muxing writes). Every boundary is rounded to the nearest
//! millisecond (ties up) ONCE, and sample durations are differences of rounded boundaries, so rounding never
//! accumulates; [`CaptionReceipt::exact`] is false when any boundary moved. Gaps between cues are empty
//! samples (a zero text length), so the track covers 0 .. the export span end. WebVTT timestamps use the
//! same rounded milliseconds.
//!
//! Text: one sample is a 16-bit big-endian byte length and the UTF-8 text (TS 26.245 §5.17 TextSample);
//! styling boxes are not written (bold / italic / underline and placement are a declared gap: the plain
//! text is carried). WebVTT cue text escapes `&`, `<` and `>` as `&amp;`, `&lt;`, `&gt;` (the only
//! characters the WebVTT cue-text parser treats as markup), and a cue whose text contains the cue
//! separator `-->` or a blank line is refused (`caption-cue-text`) instead of being silently rewritten.
//!
//! Hostile input (V4-01/02): cue count, text bytes per cue and in total, and the span are bounded by
//! [`CaptionLimits`] and checked before any byte is written; cues must be ordered, non-overlapping, inside
//! the span and non-empty in time (`caption-cue-order`, `caption-cue-overlap`, `caption-cue-span`,
//! `caption-cue-empty`); the language must be three lower-case ASCII letters (ISO 639-2/T).
#![deny(clippy::arithmetic_side_effects)]

use crate::error::ReelError;

/// Ticks per millisecond (254016000000 / 1000; test `captions_embed_as_timed_text_and_publish_a_webvtt_sidecar`).
pub const TICKS_PER_MS: u64 = 254_016_000;
/// Media timescale of the timed-text track.
pub const CAPTION_TIMESCALE: u32 = 1000;

fn unsupported(what: &'static str) -> ReelError {
    ReelError::Unsupported { what }
}
fn limit(what: &'static str) -> ReelError {
    ReelError::LimitExceeded { what }
}

/// One timed caption cue in span-relative ticks (`start < end`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptionCue {
    pub start_ticks: u64,
    pub end_ticks: u64,
    pub text: String,
}

/// Caption bounds, checked before anything is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptionLimits {
    pub max_cues: usize,
    /// Bytes of one cue's text (the sample length field is 16 bits: at most 65535).
    pub max_cue_bytes: usize,
    pub max_total_bytes: usize,
}

impl Default for CaptionLimits {
    fn default() -> Self {
        Self {
            max_cues: 100_000,
            max_cue_bytes: 4096,
            max_total_bytes: 64 * 1024 * 1024,
        }
    }
}

/// What an export carries for one caption stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportCaptions {
    /// ISO 639-2/T language, three lower-case letters (`und` when unknown).
    pub language: String,
    pub cues: Vec<CaptionCue>,
    /// Embed a `tx3g` timed-text track in the media file.
    pub embed: bool,
    /// Publish a WebVTT sidecar beside the media file (same name, extension `vtt`).
    pub sidecar: bool,
    pub limits: CaptionLimits,
}

/// What was written for one caption stream (in the export receipt).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptionReceipt {
    pub language: String,
    pub cues: u64,
    /// Samples of the embedded track (cues plus gap samples), 0 when not embedded.
    pub track_samples: u64,
    /// SHA-256 (lower-case hex) of the published WebVTT sidecar.
    pub sidecar_sha256: Option<String>,
    /// No boundary needed rounding to the millisecond.
    pub exact: bool,
}

/// One timed-text sample: duration in [`CAPTION_TIMESCALE`] units and the encoded sample bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextSample {
    pub duration: u32,
    pub data: Vec<u8>,
}

/// Rounded milliseconds of a tick count (nearest, ties up) and whether it was exact.
pub fn ms_of(ticks: u64) -> (u64, bool) {
    let q = ticks.checked_div(TICKS_PER_MS).unwrap_or(0);
    let r = ticks.checked_rem(TICKS_PER_MS).unwrap_or(0);
    let up = r.checked_mul(2).is_some_and(|d| d >= TICKS_PER_MS);
    (if up { q.saturating_add(1) } else { q }, r == 0)
}

/// Checks `captions` against a span of `span_ticks` (module docs).
pub fn validate(captions: &ExportCaptions, span_ticks: u64) -> Result<(), ReelError> {
    let lang = captions.language.as_bytes();
    if lang.len() != 3 || !lang.iter().all(u8::is_ascii_lowercase) {
        return Err(unsupported("caption-language"));
    }
    let l = &captions.limits;
    if captions.cues.len() > l.max_cues {
        return Err(limit("caption-cues"));
    }
    if l.max_cue_bytes > usize::from(u16::MAX) {
        return Err(limit("caption-cue-bytes"));
    }
    let mut total = 0usize;
    let mut prev_end = 0u64;
    for (i, c) in captions.cues.iter().enumerate() {
        if c.text.len() > l.max_cue_bytes {
            return Err(limit("caption-cue-bytes"));
        }
        total = total.checked_add(c.text.len()).ok_or(limit("caption-total-bytes"))?;
        if total > l.max_total_bytes {
            return Err(limit("caption-total-bytes"));
        }
        if c.end_ticks <= c.start_ticks {
            return Err(unsupported("caption-cue-empty"));
        }
        if c.end_ticks > span_ticks {
            return Err(unsupported("caption-cue-span"));
        }
        if i > 0 && c.start_ticks < prev_end {
            return Err(unsupported(if c.start_ticks < captions.cues[i.saturating_sub(1)].start_ticks {
                "caption-cue-order"
            } else {
                "caption-cue-overlap"
            }));
        }
        if c.text.contains("-->") || c.text.contains("\n\n") || c.text.contains("\r\n\r\n") {
            return Err(unsupported("caption-cue-text"));
        }
        prev_end = c.end_ticks;
    }
    Ok(())
}

fn sample(text: &str) -> Result<Vec<u8>, ReelError> {
    let len = u16::try_from(text.len()).map_err(|_| limit("caption-cue-bytes"))?;
    let mut out = Vec::with_capacity(text.len().saturating_add(2));
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(text.as_bytes());
    Ok(out)
}

/// The timed-text samples of validated `captions` over `0 .. span_ticks` (gaps are empty samples; a cue
/// that rounds to zero milliseconds is dropped from the track, never stretched) and whether every boundary
/// was exact.
pub fn text_samples(captions: &ExportCaptions, span_ticks: u64) -> Result<(Vec<TextSample>, bool), ReelError> {
    validate(captions, span_ticks)?;
    let mut out = Vec::with_capacity(captions.cues.len().saturating_mul(2).saturating_add(1));
    let mut exact = true;
    let mut at = 0u64;
    let push = |out: &mut Vec<TextSample>, from: u64, to: u64, text: &str| -> Result<(), ReelError> {
        if to > from {
            let d = u32::try_from(to.checked_sub(from).ok_or(limit("caption-time"))?).map_err(|_| limit("caption-time"))?;
            out.push(TextSample { duration: d, data: sample(text)? });
        }
        Ok(())
    };
    for c in &captions.cues {
        let (s, e1) = ms_of(c.start_ticks);
        let (e, e2) = ms_of(c.end_ticks);
        exact &= e1 && e2;
        let s = s.max(at);
        push(&mut out, at, s, "")?;
        push(&mut out, s, e, &c.text)?;
        at = at.max(e);
    }
    let (end, e3) = ms_of(span_ticks);
    exact &= e3;
    push(&mut out, at, end, "")?;
    Ok((out, exact))
}

fn timestamp(ms: u64) -> String {
    let split = |v: u64, d: u64| (v.checked_div(d).unwrap_or(0), v.checked_rem(d).unwrap_or(0));
    let (h, rest) = split(ms, 3_600_000);
    let (m, rest) = split(rest, 60_000);
    let (s, f) = split(rest, 1000);
    format!("{h:02}:{m:02}:{s:02}.{f:03}")
}

/// The WebVTT file of validated `captions` (module docs).
pub fn webvtt(captions: &ExportCaptions, span_ticks: u64) -> Result<String, ReelError> {
    validate(captions, span_ticks)?;
    let mut out = String::from("WEBVTT\n");
    for c in &captions.cues {
        let (s, _) = ms_of(c.start_ticks);
        let (e, _) = ms_of(c.end_ticks);
        if e <= s {
            continue;
        }
        let text = c.text.replace("\r\n", "\n").replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        out.push('\n');
        out.push_str(&timestamp(s));
        out.push_str(" --> ");
        out.push_str(&timestamp(e));
        out.push('\n');
        out.push_str(&text);
        out.push('\n');
    }
    Ok(out)
}
