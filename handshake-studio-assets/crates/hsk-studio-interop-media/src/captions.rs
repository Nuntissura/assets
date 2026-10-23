//! Caption interchange (lane SMB-PREMIERE-COLOR-TITLES slice 4): SubRip (`.srt`) and WebVTT
//! (`.vtt`) read into and written from Folio caption segments (`StudioCaptionSegment`,
//! STU-VID-037 "A caption track holds caption segments with text, timing and styling"). Which
//! formats Studio reads and writes is the declared spec gap STU-VID-085; these two are the
//! formats Premiere imports and exports as sidecars (Premiere parity UNVERIFIED, D5).
//!
//! Times are exact: one millisecond is 254 016 000 ticks (STU-VID-012), so a cue time maps to ticks
//! without rounding. Styling is the Studio subset of `StudioCaptionStyle`: a `<b>`, `<i>` or `<u>`
//! span covering the WHOLE cue becomes the segment flag; SRT `{\anN}` (N in 7..=9 = top) and a
//! WebVTT `line:` setting (`0`..`49%` or a non-negative line number = top) set the placement;
//! WebVTT `align:` / `position:` set the alignment; SRT `<font color="#rrggbb">` around the whole
//! cue sets the colour. Every other tag (`<c.x>`, `<v Speaker>`, `<lang>`, `<ruby>`, `<rt>`,
//! inline timestamps, partial spans) is removed and its text kept.
//!
//! Hostile text (workflow V4-08): the text is tokenised, never substring-matched: line endings
//! CRLF / CR / LF are normalised first, a WebVTT body is split into blocks at blank lines, `NOTE`,
//! `STYLE` and `REGION` blocks are skipped whole (a `-->` inside a NOTE is a decoy, not a cue),
//! timestamps are parsed field by field (minutes and seconds `< 60`, milliseconds exactly three
//! digits, hours bounded), tags are scanned as `<...>` tokens and WebVTT character references
//! (`&amp;` `&lt;` `&gt;` `&quot;` `&apos;` `&nbsp;` `&lrm;` `&rlm;`, decimal / hex) are decoded
//! after tag removal; tag names are case-insensitive. Bounds (V4-01/02): the input size, the cue
//! count and each cue's text are checked against [`CaptionLimits`] before they are stored; integer
//! arithmetic is checked. Writers escape what they emit (`&`, `<`, `>` in WebVTT; in SRT a `-->`
//! inside cue text is written `-- >` so it cannot read as a timing line, and blank lines inside a
//! cue are dropped because a blank line ends a cue in both formats).
#![deny(clippy::arithmetic_side_effects)]

use hsk_studio_folio::{StudioCaptionAlign, StudioCaptionPlacement, StudioCaptionSegment, StudioCaptionStyle};
use std::fmt::Write as _;

/// Ticks per millisecond (254 016 000 000 / 1000).
pub const TICKS_PER_MS: u64 = 254_016_000;
/// Largest hour field read (a 10 000-hour programme).
pub const MAX_CAPTION_HOURS: u64 = 9_999;

/// Bounds of one caption import.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptionLimits {
    pub max_bytes: usize,
    pub max_cues: usize,
    pub max_cue_text_bytes: usize,
}
impl Default for CaptionLimits {
    fn default() -> Self {
        Self {
            max_bytes: 16 * 1024 * 1024,
            max_cues: hsk_studio_folio::MAX_CAPTION_SEGMENTS,
            max_cue_text_bytes: hsk_studio_folio::MAX_CAPTION_TEXT_BYTES,
        }
    }
}

/// A typed caption refusal (`code` stable; `line` 1-based, 0 when not tied to a line).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptionError {
    pub code: &'static str,
    pub line: usize,
}
impl std::fmt::Display for CaptionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "caption {} at line {}", self.code, self.line)
    }
}
impl std::error::Error for CaptionError {}
fn fail(code: &'static str, line: usize) -> CaptionError {
    CaptionError { code, line }
}

pub const CAPTION_TOO_LARGE: &str = "caption_too_large";
pub const CAPTION_TOO_MANY_CUES: &str = "caption_too_many_cues";
pub const CAPTION_CUE_TOO_LONG: &str = "caption_cue_too_long";
pub const CAPTION_TIMESTAMP: &str = "caption_timestamp";
pub const CAPTION_TIMING: &str = "caption_timing";
pub const CAPTION_SYNTAX: &str = "caption_syntax";
pub const CAPTION_HEADER: &str = "caption_header";
pub const CAPTION_ENCODING: &str = "caption_encoding";

fn lines_of(text: &str) -> Vec<&str> {
    // CRLF and lone CR both end a line.
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        match rest.find(['\r', '\n']) {
            Some(i) => {
                out.push(&rest[..i]);
                let skip = if rest[i..].starts_with("\r\n") { 2 } else { 1 };
                rest = &rest[i.saturating_add(skip)..];
            }
            None => {
                out.push(rest);
                rest = "";
            }
        }
    }
    out
}

/// Blocks separated by blank lines: `(first line number, lines)`.
fn blocks<'a>(lines: &[&'a str]) -> Vec<(usize, Vec<&'a str>)> {
    let mut out = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    let mut start = 0usize;
    for (i, l) in lines.iter().enumerate() {
        if l.trim().is_empty() {
            if !cur.is_empty() {
                out.push((start, std::mem::take(&mut cur)));
            }
        } else {
            if cur.is_empty() {
                start = i.saturating_add(1);
            }
            cur.push(l);
        }
    }
    if !cur.is_empty() {
        out.push((start, cur));
    }
    out
}

fn digits(s: &str, n: Option<usize>) -> Option<u64> {
    if s.is_empty() || n.is_some_and(|n| s.len() != n) || !s.bytes().all(|b| b.is_ascii_digit()) || s.len() > 6 {
        return None;
    }
    s.parse().ok()
}

/// `[HH:]MM:SS<sep>mmm` to ticks; `hours_required` for SRT.
fn timestamp(s: &str, sep: char, hours_required: bool) -> Option<u64> {
    let (clock, ms) = s.split_once(sep)?;
    let ms = digits(ms, Some(3))?;
    let parts: Vec<&str> = clock.split(':').collect();
    let (h, m, sec) = match parts.as_slice() {
        [h, m, s] => (digits(h, None)?, digits(m, Some(2))?, digits(s, Some(2))?),
        [m, s] if !hours_required => (0, digits(m, Some(2))?, digits(s, Some(2))?),
        _ => return None,
    };
    if m >= 60 || sec >= 60 || h > MAX_CAPTION_HOURS {
        return None;
    }
    let total_ms = h.checked_mul(3_600_000)?.checked_add(m.checked_mul(60_000)?)?.checked_add(sec.checked_mul(1000)?)?.checked_add(ms)?;
    total_ms.checked_mul(TICKS_PER_MS)
}

/// The timing line `start --> end[ settings]`.
fn timing(line: &str, sep: char, hours_required: bool, n: usize) -> Result<(u64, u64, &str), CaptionError> {
    let (a, rest) = line.split_once("-->").ok_or(fail(CAPTION_SYNTAX, n))?;
    let rest = rest.trim_start();
    let (b, settings) = rest.split_once(|c: char| c.is_ascii_whitespace()).unwrap_or((rest, ""));
    let start = timestamp(a.trim(), sep, hours_required).ok_or(fail(CAPTION_TIMESTAMP, n))?;
    let end = timestamp(b.trim(), sep, hours_required).ok_or(fail(CAPTION_TIMESTAMP, n))?;
    if end <= start {
        return Err(fail(CAPTION_TIMING, n));
    }
    Ok((start, end, settings))
}

/// One `<...>` tag token.
struct Tag<'a> {
    closing: bool,
    name: String,
    rest: &'a str,
}

/// Splits text into tags and text runs. A tag is `<` then an optional `/` then an ASCII letter or
/// digit, up to the next `>` with no `<` inside; any other `<` (a lone `<`, `< b`, an unterminated
/// tag) is text.
fn tokens(text: &str) -> Vec<Result<Tag<'_>, &str>> {
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let tag_end = || -> Option<usize> {
            let after = rest.get(1..)?;
            let name = after.strip_prefix('/').unwrap_or(after);
            if !name.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric()) {
                return None;
            }
            let end = rest.find('>')?;
            (!rest.get(1..end)?.contains('<')).then_some(end)
        };
        match rest.find('<') {
            Some(0) => match tag_end() {
                Some(end) => {
                    let inner = &rest[1..end];
                    let (closing, body) = inner.strip_prefix('/').map_or((false, inner), |b| (true, b));
                    let name_end = body.find(|c: char| c.is_ascii_whitespace() || c == '.').unwrap_or(body.len());
                    out.push(Ok(Tag { closing, name: body[..name_end].to_ascii_lowercase(), rest: &body[name_end..] }));
                    rest = &rest[end.saturating_add(1)..];
                }
                None => {
                    out.push(Err(&rest[..1]));
                    rest = &rest[1..];
                }
            },
            Some(i) => {
                out.push(Err(&rest[..i]));
                rest = &rest[i..];
            }
            None => {
                out.push(Err(rest));
                rest = "";
            }
        }
    }
    out
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(semi) = tail.get(..tail.len().min(12)).and_then(|t| t.find(';')) else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let name = &tail[1..semi];
        let ch = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            "lrm" => Some('\u{200e}'),
            "rlm" => Some('\u{200f}'),
            _ => name
                .strip_prefix("#x")
                .or_else(|| name.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32)
                .filter(|c| !c.is_control() || *c == '\n'),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &tail[semi.saturating_add(1)..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn colour_attr(rest: &str) -> Option<[f64; 4]> {
    let lower = rest.to_ascii_lowercase();
    let at = lower.find("color")?;
    let v = lower[at..].split_once('=')?.1.trim_start().trim_start_matches(['"', '\'']);
    let hex = v.strip_prefix('#')?.get(..6)?;
    let c = |i: usize| u8::from_str_radix(hex.get(i..i.saturating_add(2))?, 16).ok().map(|v| f64::from(v) / 255.0);
    Some([c(0)?, c(2)?, c(4)?, 1.0])
}

/// Cue text to `(plain text, style)`: whole-cue spans become flags, other tags vanish.
fn cue_text(lines: &[&str], vtt: bool, style: &mut StudioCaptionStyle) -> String {
    let joined = lines.join("\n");
    let mut toks = tokens(&joined);
    // Peel whole-cue wrappers: first token an opening b/i/u/font tag, last its closing tag.
    while let (Some(Ok(first)), Some(Ok(last))) = (toks.first(), toks.last()) {
        if toks.len() < 2 || first.closing || !last.closing || first.name != last.name {
            break;
        }
        match first.name.as_str() {
            "b" => style.bold = true,
            "i" => style.italic = true,
            "u" => style.underline = true,
            "font" if !vtt => style.colour = colour_attr(first.rest).or(style.colour),
            _ => break,
        }
        let _ = toks.remove(0);
        let _ = toks.pop();
    }
    let mut text = String::new();
    for t in toks {
        if let Err(s) = t {
            text.push_str(s);
        }
    }
    if !vtt {
        // SubStation position overrides: `{\an8}` and friends.
        while let Some(i) = text.find("{\\an") {
            let tail = &text[i..];
            let Some(end) = tail.find('}') else { break };
            if let Some(d) = tail.get(4..end).and_then(|n| n.parse::<u8>().ok()) {
                if (7..=9).contains(&d) {
                    style.placement = StudioCaptionPlacement::Top;
                }
                style.align = match d.checked_rem(3).unwrap_or(2) {
                    1 => StudioCaptionAlign::Left,
                    0 => StudioCaptionAlign::Right,
                    _ => StudioCaptionAlign::Center,
                };
            }
            text.replace_range(i..i.saturating_add(end).saturating_add(1), "");
        }
    }
    if vtt { decode_entities(&text) } else { text }
}

fn push_cue(
    out: &mut Vec<StudioCaptionSegment>,
    seg: StudioCaptionSegment,
    limits: &CaptionLimits,
    line: usize,
) -> Result<(), CaptionError> {
    if out.len() >= limits.max_cues {
        return Err(fail(CAPTION_TOO_MANY_CUES, line));
    }
    if seg.text.len() > limits.max_cue_text_bytes {
        return Err(fail(CAPTION_CUE_TOO_LONG, line));
    }
    if seg.text.chars().any(|c| c.is_control() && c != '\n') {
        return Err(fail(CAPTION_SYNTAX, line));
    }
    out.push(seg);
    Ok(())
}

fn text_of(bytes: &[u8], limits: &CaptionLimits) -> Result<String, CaptionError> {
    if bytes.len() > limits.max_bytes {
        return Err(fail(CAPTION_TOO_LARGE, 0));
    }
    let s = std::str::from_utf8(bytes).map_err(|_| fail(CAPTION_ENCODING, 0))?;
    Ok(s.strip_prefix('\u{feff}').unwrap_or(s).to_owned())
}

fn sort(mut v: Vec<StudioCaptionSegment>) -> Vec<StudioCaptionSegment> {
    v.sort_by_key(|s| s.start);
    v
}

/// Reads SubRip text (module docs). Segments are returned sorted by start.
pub fn parse_srt(bytes: &[u8], limits: &CaptionLimits) -> Result<Vec<StudioCaptionSegment>, CaptionError> {
    let text = text_of(bytes, limits)?;
    let lines = lines_of(&text);
    let mut out = Vec::new();
    for (n, block) in blocks(&lines) {
        // Optional numeric counter, then the timing line, then the text.
        let (timing_at, first) = match block.first().map(|l| l.trim()) {
            Some(l) if !l.contains("-->") => (1usize, l),
            _ => (0, ""),
        };
        if timing_at == 1 && digits(first, None).is_none() {
            return Err(fail(CAPTION_SYNTAX, n));
        }
        let line_no = n.saturating_add(timing_at);
        let t = block.get(timing_at).ok_or(fail(CAPTION_SYNTAX, n))?;
        let (start, end, _) = timing(t, ',', true, line_no)?;
        let mut style = StudioCaptionStyle::default();
        let body = block.get(timing_at.saturating_add(1)..).unwrap_or(&[]);
        let text = cue_text(body, false, &mut style);
        push_cue(&mut out, StudioCaptionSegment { start, end, text, style }, limits, line_no)?;
    }
    Ok(sort(out))
}

fn vtt_settings(settings: &str, style: &mut StudioCaptionStyle) {
    for s in settings.split_ascii_whitespace() {
        let Some((k, v)) = s.split_once(':') else { continue };
        match k {
            "line" => {
                let v = v.split(',').next().unwrap_or(v);
                let top = match v.strip_suffix('%') {
                    Some(p) => p.parse::<f64>().is_ok_and(|p| p < 50.0),
                    None => v.parse::<i64>().is_ok_and(|n| n >= 0),
                };
                if top {
                    style.placement = StudioCaptionPlacement::Top;
                }
            }
            "align" => {
                style.align = match v {
                    "left" | "start" => StudioCaptionAlign::Left,
                    "right" | "end" => StudioCaptionAlign::Right,
                    _ => StudioCaptionAlign::Center,
                }
            }
            _ => {}
        }
    }
}

/// Reads WebVTT text (module docs). Segments are returned sorted by start.
pub fn parse_vtt(bytes: &[u8], limits: &CaptionLimits) -> Result<Vec<StudioCaptionSegment>, CaptionError> {
    let text = text_of(bytes, limits)?;
    let lines = lines_of(&text);
    let header = lines.first().copied().unwrap_or("");
    let ok = header == "WEBVTT" || header.strip_prefix("WEBVTT").is_some_and(|r| r.starts_with([' ', '\t']));
    if !ok {
        return Err(fail(CAPTION_HEADER, 1));
    }
    let mut out = Vec::new();
    for (i, (n, block)) in blocks(&lines).into_iter().enumerate() {
        if i == 0 {
            continue; // the header block (WEBVTT plus header lines)
        }
        let head = block.first().copied().unwrap_or("");
        let word = head.split([' ', '\t']).next().unwrap_or("");
        if matches!(word, "NOTE" | "STYLE" | "REGION") {
            continue;
        }
        let timing_at = if head.contains("-->") { 0usize } else { 1 };
        let line_no = n.saturating_add(timing_at);
        let t = block.get(timing_at).ok_or(fail(CAPTION_SYNTAX, n))?;
        let (start, end, settings) = timing(t, '.', false, line_no)?;
        let mut style = StudioCaptionStyle::default();
        vtt_settings(settings, &mut style);
        let body = block.get(timing_at.saturating_add(1)..).unwrap_or(&[]);
        let text = cue_text(body, true, &mut style);
        push_cue(&mut out, StudioCaptionSegment { start, end, text, style }, limits, line_no)?;
    }
    Ok(sort(out))
}

fn clock(ticks: u64, sep: char, out: &mut String) -> Result<(), CaptionError> {
    let div = |a: u64, b: u64| a.checked_div(b).unwrap_or(0);
    let rem = |a: u64, b: u64| a.checked_rem(b).unwrap_or(0);
    if rem(ticks, TICKS_PER_MS) != 0 {
        return Err(fail(CAPTION_TIMING, 0));
    }
    let ms = div(ticks, TICKS_PER_MS);
    let (h, r) = (div(ms, 3_600_000), rem(ms, 3_600_000));
    if h > MAX_CAPTION_HOURS {
        return Err(fail(CAPTION_TIMESTAMP, 0));
    }
    let _ = write!(out, "{h:02}:{:02}:{:02}{sep}{:03}", div(r, 60_000), div(rem(r, 60_000), 1000), rem(r, 1000));
    Ok(())
}

fn wrap(text: String, style: &StudioCaptionStyle, colour: bool) -> String {
    let mut t = text;
    for (on, tag) in [(style.underline, "u"), (style.italic, "i"), (style.bold, "b")] {
        if on {
            t = format!("<{tag}>{t}</{tag}>");
        }
    }
    if colour && let Some(c) = style.colour {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        t = format!("<font color=\"#{:02x}{:02x}{:02x}\">{t}</font>", b(c[0]), b(c[1]), b(c[2]));
    }
    t
}

/// Writes SubRip text. A segment time that is not a whole millisecond is refused
/// `caption_timing` (never rounded); placement top writes `{\an8}`.
pub fn write_srt(segments: &[StudioCaptionSegment]) -> Result<String, CaptionError> {
    let mut out = String::new();
    for (i, s) in segments.iter().enumerate() {
        let _ = writeln!(out, "{}", i.saturating_add(1));
        clock(s.start, ',', &mut out)?;
        out.push_str(" --> ");
        clock(s.end, ',', &mut out)?;
        out.push('\n');
        // A blank line would end the cue; an arrow would read as a timing line.
        let body: String = s.text.split('\n').filter(|l| !l.trim().is_empty()).collect::<Vec<_>>().join("\n").replace("-->", "-- >");
        let mut body = wrap(body, &s.style, true);
        if s.style.placement == StudioCaptionPlacement::Top {
            body.insert_str(0, "{\\an8}");
        }
        out.push_str(&body);
        out.push_str("\n\n");
    }
    Ok(out)
}

/// Writes WebVTT text (`&`, `<`, `>` escaped; placement and alignment as cue settings).
pub fn write_vtt(segments: &[StudioCaptionSegment]) -> Result<String, CaptionError> {
    let mut out = String::from("WEBVTT\n\n");
    for s in segments {
        clock(s.start, '.', &mut out)?;
        out.push_str(" --> ");
        clock(s.end, '.', &mut out)?;
        if s.style.placement == StudioCaptionPlacement::Top {
            out.push_str(" line:0");
        }
        match s.style.align {
            StudioCaptionAlign::Left => out.push_str(" align:left"),
            StudioCaptionAlign::Right => out.push_str(" align:right"),
            StudioCaptionAlign::Center => {}
        }
        out.push('\n');
        let escaped: String = s
            .text
            .split('\n')
            .filter(|l| !l.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n")
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        out.push_str(&wrap(escaped, &s.style, false));
        out.push_str("\n\n");
    }
    Ok(out)
}
