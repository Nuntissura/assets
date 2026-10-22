//! Content-stream tokenizer and strict operator parser (ISO 32000-2 7.8.2, 8.9.7, Annex A). Produces
//! operations with their source byte spans; nothing is executed or interpreted here. For `BI` the
//! operands are the key/value objects of the inline image dictionary and `inline_data` is the exact
//! byte range of its data.
//!
//! Lead decision 2026-10-11T15:00:20Z (press R10 pre-review, CANONICAL REWRITE): a viewer and this crate
//! must read the same operators. Where viewers disagree with each other or with ISO 32000 the parser
//! does not guess, it refuses (`PressError::Malformed`), and the caller treats the stream as content it
//! cannot read:
//! - [`parse_strict`]: every operator is one of the ISO 32000-2 Annex A operators with EXACTLY its
//!   operand count and operand types ([`check_op`]; counts as in pdf.js `EvaluatorPreprocessor.opMap`
//!   at da95557, which fills a short operator from earlier extra operands and drops the FIRST extra
//!   ones, R10-F5); an unknown operator, an extra or missing operand, or an operand of the wrong type
//!   is a refusal, never "first wins" or "last wins";
//! - inline images end where their data ends, not at the first `EI` that looks plausible (R10-F1):
//!   unfiltered data has exactly `/H` rows of `ceil(/W x components x /BPC / 8)` bytes, filtered data
//!   ends where the first filter's end-of-data marker is (Flate, LZW, ASCIIHex, ASCII85, RunLength:
//!   `filters::first_stage_extent`), data whose first filter is a raster codec ends at the first `EI`
//!   before which the data has that codec's complete structure (`raster::codec_signature`); then only
//!   white space may come before `EI`, and `EI` must be followed by white space, a delimiter or the
//!   end. Exactly one white-space byte follows `ID`. Operands before `BI` are a refusal (pdf.js would
//!   hand them to a later operator). [`Parsed::inline_ambiguous`] reports data that holds an `EI`
//!   followed by white space, where pdf.js's heuristic end search (parser.js
//!   `findDefaultInlineStreamEnd`) may stop early: the proof treats such content as unreadable, and the
//!   redaction's canonical rewrite turns every inline image into an image XObject, so no saved content
//!   carries `BI`/`ID`/`EI` at all (`canon.rs`).
//!
//! [`tokenize`] is the lenient form (any operator name, any operand count) used only to decide whether
//! a stream no content role reaches looks like content.
#![deny(clippy::arithmetic_side_effects)]
use crate::error::{Limits, PressError, Result};
use crate::syntax::{Dict, Lexer, Object, is_regular, is_ws};

/// Operands one operation may carry (an inline image dictionary counts key and value objects).
pub(crate) const MAX_OPERANDS: usize = 256;

#[derive(Clone, Debug)]
pub struct Op {
    pub name: Vec<u8>,
    pub operands: Vec<Object>,
    /// Byte span (operands through operator) in the tokenized data.
    pub start: usize,
    pub end: usize,
    /// For `BI`: the exact byte range of the image data.
    pub inline_data: Option<(usize, usize)>,
}

/// Number of colour components of an inline image's NAMED colour space, looked up in the
/// `/ColorSpace` resources of the stream (`None`: not known).
pub(crate) type ColorSpaces<'a> = &'a dyn Fn(&[u8]) -> Option<usize>;

/// A colour-space lookup that knows no resource names.
pub(crate) fn no_color_spaces(_: &[u8]) -> Option<usize> {
    None
}

/// The operations of one content stream.
#[derive(Clone, Debug, Default)]
pub(crate) struct Parsed {
    pub ops: Vec<Op>,
    /// Operands are left after the last operator. A viewer joins a page's content streams, so they
    /// belong to the first operator of the next stream.
    pub trailing_operands: bool,
    /// The stream ends inside a comment (no end-of-line after its last `%`): joined to the next
    /// stream of a page, the comment would swallow that stream's first line.
    pub ends_in_comment: bool,
    /// Some inline image's data holds `EI` followed by white space (see the module docs).
    pub inline_ambiguous: bool,
}

/// Lenient tokenization: any operator name and operand count, and an inline image whose data end
/// cannot be computed ends at the first `EI` preceded by white space (module docs).
pub(crate) fn tokenize(data: &[u8], limits: &Limits, cs: ColorSpaces<'_>, budget: &mut usize) -> Result<Parsed> {
    tokenize_with(data, limits, cs, budget, true)
}

fn tokenize_with(data: &[u8], limits: &Limits, cs: ColorSpaces<'_>, budget: &mut usize, lenient: bool) -> Result<Parsed> {
    let mut lx = Lexer::new(data, 0, limits.max_depth);
    let mut out = Parsed::default();
    loop {
        out.ends_in_comment = skip(&mut lx);
        if lx.peek().is_none() {
            break;
        }
        let start = lx.pos;
        let mut operands: Vec<Object> = Vec::new();
        let name = loop {
            out.ends_in_comment = skip(&mut lx);
            let Some(b) = lx.peek() else {
                out.trailing_operands = !operands.is_empty();
                return Ok(out);
            };
            match b {
                b'/' | b'(' | b'<' | b'[' | b'+' | b'-' | b'.' | b'0'..=b'9' => {
                    operands.push(lx.parse_object(0, false)?);
                    if operands.len() > MAX_OPERANDS {
                        return Err(PressError::InputLimit("operands"));
                    }
                }
                _ if is_regular(b) => {
                    let kw = lx.read_keyword();
                    match kw {
                        b"true" => operands.push(Object::Bool(true)),
                        b"false" => operands.push(Object::Bool(false)),
                        b"null" => operands.push(Object::Null),
                        _ => break kw.to_vec(),
                    }
                }
                _ => return Err(PressError::Malformed("content_token")),
            }
        };
        let mut inline_data = None;
        if name == b"BI" {
            if !operands.is_empty() {
                return Err(PressError::Malformed("operands_before_inline_image"));
            }
            let (range, ambiguous) = inline_image(&mut lx, data, &mut operands, limits, cs, budget, lenient)?;
            inline_data = Some(range);
            out.inline_ambiguous |= ambiguous;
        }
        out.ops.push(Op { name, operands, start, end: lx.pos, inline_data });
        if out.ops.len() > limits.max_content_ops {
            return Err(PressError::InputLimit("max_content_ops"));
        }
    }
    Ok(out)
}

/// [`tokenize`] and every operation checked by [`check_op`] (module docs).
pub(crate) fn parse_strict(data: &[u8], limits: &Limits, cs: ColorSpaces<'_>, budget: &mut usize) -> Result<Parsed> {
    let parsed = tokenize_with(data, limits, cs, budget, false)?;
    for op in &parsed.ops {
        check_op(op)?;
    }
    Ok(parsed)
}

/// White space and comments; `true` when a comment ran to the end of the data without an end-of-line.
fn skip(lx: &mut Lexer<'_>) -> bool {
    let mut open_comment = false;
    loop {
        while lx.peek().is_some_and(is_ws) {
            open_comment = false;
            lx.advance(1);
        }
        if lx.peek() == Some(b'%') {
            open_comment = true;
            while lx.peek().is_some_and(|b| !matches!(b, 10 | 13)) {
                lx.advance(1);
            }
        } else {
            return open_comment && lx.peek().is_none();
        }
    }
}

/// Two content streams of one page join without changing their tokens: a viewer concatenates them
/// byte for byte (pdf.js `StreamsSequenceStream` at da95557 inserts no separator), so a stream that
/// ends in a regular character followed by one that starts with one would merge two tokens
/// (`Tj` + `BT` = `TjBT`), and a comment left open at the end swallows the next stream's first line.
pub(crate) fn joins_cleanly(prev: &[u8], prev_parsed: &Parsed, next: &[u8]) -> bool {
    if prev_parsed.ends_in_comment && !next.is_empty() {
        return false;
    }
    !(prev.last().is_some_and(|b| is_regular(*b)) && next.first().is_some_and(|b| is_regular(*b)))
}

/// Full name of an inline-image dictionary key (ISO 32000-2 8.9.7 Table 91); other keys as they are.
pub(crate) fn inline_key(k: &[u8]) -> &[u8] {
    match k {
        b"BPC" => b"BitsPerComponent",
        b"CS" => b"ColorSpace",
        b"D" => b"Decode",
        b"DP" => b"DecodeParms",
        b"F" => b"Filter",
        b"H" => b"Height",
        b"IM" => b"ImageMask",
        b"I" => b"Interpolate",
        b"W" => b"Width",
        b"L" => b"Length",
        other => other,
    }
}

/// Full name of an abbreviated inline-image VALUE name (ISO 32000-2 8.9.7 Table 92: colour spaces
/// and filters); other names as they are.
pub(crate) fn inline_value_name(n: &[u8]) -> &[u8] {
    match n {
        b"G" => b"DeviceGray",
        b"RGB" => b"DeviceRGB",
        b"CMYK" => b"DeviceCMYK",
        b"I" => b"Indexed",
        b"AHx" => b"ASCIIHexDecode",
        b"A85" => b"ASCII85Decode",
        b"LZW" => b"LZWDecode",
        b"Fl" => b"FlateDecode",
        b"RL" => b"RunLengthDecode",
        b"CCF" => b"CCITTFaxDecode",
        b"DCT" => b"DCTDecode",
        other => other,
    }
}

/// The inline image dictionary with full key names (values unchanged; the last of a repeated key
/// wins, as for any dictionary).
pub(crate) fn inline_dict(operands: &[Object]) -> Dict {
    let mut d = Dict::default();
    for pair in operands.chunks(2) {
        if let [Object::Name(k), v] = pair {
            d.set_key(inline_key(k), v.clone());
        }
    }
    d
}

/// Colour components of an inline image colour space value (device names, abbreviations, colour
/// space arrays; a resource name through `cs`).
pub(crate) fn components(v: &Object, cs: ColorSpaces<'_>) -> Option<usize> {
    match v {
        Object::Name(n) => match inline_value_name(n) {
            b"DeviceGray" | b"CalGray" | b"Indexed" | b"Separation" => Some(1),
            b"DeviceRGB" | b"CalRGB" | b"Lab" => Some(3),
            b"DeviceCMYK" => Some(4),
            other => cs(other),
        },
        Object::Array(a) => match a.first().and_then(Object::as_name).map(inline_value_name) {
            Some(b"Indexed" | b"CalGray" | b"Separation") => Some(1),
            Some(b"CalRGB" | b"Lab") => Some(3),
            Some(b"DeviceN") => a.get(1).and_then(Object::as_array).map(<[Object]>::len).filter(|n| *n > 0),
            Some(b"DeviceGray") if a.len() == 1 => Some(1),
            Some(b"DeviceRGB") if a.len() == 1 => Some(3),
            Some(b"DeviceCMYK") if a.len() == 1 => Some(4),
            _ => None,
        },
        _ => None,
    }
}

/// Filter names of an inline image (`/F` or `/Filter`; name or array of names), expanded.
fn inline_filters(d: &Dict) -> Result<Vec<Vec<u8>>> {
    match d.get("Filter") {
        None | Some(Object::Null) => Ok(Vec::new()),
        Some(Object::Name(n)) => Ok(vec![inline_value_name(n).to_vec()]),
        Some(Object::Array(a)) => a.iter().map(|o| o.as_name().map(|n| inline_value_name(n).to_vec()).ok_or(PressError::Malformed("inline_image_filter"))).collect(),
        Some(_) => Err(PressError::Malformed("inline_image_filter")),
    }
}

fn positive(d: &Dict, key: &str) -> Result<usize> {
    d.get(key).and_then(Object::as_int).and_then(|v| usize::try_from(v).ok()).filter(|v| *v > 0).ok_or(PressError::Malformed("inline_image_dims"))
}

/// Exact byte length of unfiltered inline image data (module docs).
fn raw_length(d: &Dict, cs: ColorSpaces<'_>, limits: &Limits) -> Result<usize> {
    let (w, h) = (positive(d, "Width")?, positive(d, "Height")?);
    let mask = matches!(d.get("ImageMask"), Some(Object::Bool(true)));
    let (bpc, n) = if mask {
        if d.get("BitsPerComponent").and_then(Object::as_int).is_some_and(|b| b != 1) {
            return Err(PressError::Malformed("inline_image_bpc"));
        }
        (1usize, 1usize)
    } else {
        let bpc = d.get("BitsPerComponent").and_then(Object::as_int).filter(|b| matches!(b, 1 | 2 | 4 | 8 | 16)).ok_or(PressError::Malformed("inline_image_bpc"))?;
        let n = d.get("ColorSpace").and_then(|v| components(v, cs)).ok_or(PressError::Malformed("inline_image_color_space"))?;
        (usize::try_from(bpc).map_err(|_| PressError::Malformed("inline_image_bpc"))?, n)
    };
    let bits = w.checked_mul(n).and_then(|x| x.checked_mul(bpc)).ok_or(PressError::InputLimit("inline_image"))?;
    let row = bits.checked_add(7).map(|x| x / 8).ok_or(PressError::InputLimit("inline_image"))?;
    let len = row.checked_mul(h).ok_or(PressError::InputLimit("inline_image"))?;
    if len > limits.max_decoded_bytes {
        return Err(PressError::InputLimit("inline_image"));
    }
    Ok(len)
}

/// `EI` at `at` is a keyword: followed by the end, white space or a delimiter.
fn ei_at(data: &[u8], at: usize) -> bool {
    data.get(at..).is_some_and(|r| r.starts_with(b"EI")) && data.get(at.saturating_add(2)).is_none_or(|b| !is_regular(*b))
}

/// Parses an inline image after `BI`: the dictionary up to `ID`, then the data and `EI`. Returns the
/// data range and whether the data holds an `EI` + white-space sequence pdf.js's heuristic could
/// take as the end.
fn inline_image(
    lx: &mut Lexer<'_>,
    data: &[u8],
    operands: &mut Vec<Object>,
    limits: &Limits,
    cs: ColorSpaces<'_>,
    budget: &mut usize,
    lenient: bool,
) -> Result<((usize, usize), bool)> {
    loop {
        skip(lx);
        if lx.starts_with(b"ID") && lx.data.get(lx.pos.saturating_add(2)).is_none_or(|b| !is_regular(*b)) {
            lx.advance(2);
            break;
        }
        if lx.peek().is_none() {
            return Err(PressError::Malformed("inline_image"));
        }
        operands.push(lx.parse_object(0, false)?);
        if operands.len() > MAX_OPERANDS {
            return Err(PressError::InputLimit("operands"));
        }
    }
    // Exactly one white-space byte separates ID from the data (ISO 32000-2 8.9.7).
    if !lx.peek().is_some_and(is_ws) {
        return Err(PressError::Malformed("inline_image_id"));
    }
    lx.advance(1);
    let at = lx.pos;
    let dict = inline_dict(operands);
    let filters = inline_filters(&dict)?;
    let end = match exact_end(data, at, &dict, &filters, limits, cs, budget) {
        Ok(end) => end,
        Err(e @ (PressError::InputLimit("decoded_total" | "decoded_bytes") | PressError::Canceled)) => return Err(e),
        Err(_) if lenient => legacy_end(data, at)?,
        Err(e) => return Err(e),
    };
    let mut q = end;
    while data.get(q).is_some_and(|b| is_ws(*b)) {
        q = q.saturating_add(1);
    }
    if !ei_at(data, q) {
        return Err(PressError::Malformed("inline_image_end"));
    }
    lx.pos = q.saturating_add(2);
    // pdf.js ends data at the first E, I, (space | LF | CR) that its look-ahead accepts, except for
    // DCT (EOI search), ASCII85 (~>) and ASCIIHex (>) data.
    let heuristic = !matches!(filters.first().map(Vec::as_slice), Some(b"DCTDecode" | b"ASCII85Decode" | b"ASCIIHexDecode"));
    let body = data.get(at..end).unwrap_or(&[]);
    let ambiguous = heuristic && body.windows(3).any(|w| w[0] == b'E' && w[1] == b'I' && matches!(w[2], b' ' | b'\n' | b'\r'));
    Ok(((at, end), ambiguous))
}

/// The first `EI` keyword preceded by white space (the reading this crate used before R10; only for
/// classifying streams no content role reaches, [`tokenize`]): the data ends before that white space.
fn legacy_end(data: &[u8], at: usize) -> Result<usize> {
    let mut p = at;
    loop {
        let rest = data.get(p..).ok_or(PressError::Malformed("inline_image"))?;
        let found = crate::reader::find(rest, b"EI").ok_or(PressError::Malformed("inline_image"))?;
        let q = p.saturating_add(found);
        if q > at && data.get(q.saturating_sub(1)).is_some_and(|b| is_ws(*b)) && ei_at(data, q) {
            return Ok(q.saturating_sub(1));
        }
        p = q.saturating_add(2);
    }
}

/// Where an inline image's data ends by its own structure (module docs).
fn exact_end(data: &[u8], at: usize, dict: &Dict, filters: &[Vec<u8>], limits: &Limits, cs: ColorSpaces<'_>, budget: &mut usize) -> Result<usize> {
    let rest = data.get(at..).unwrap_or(&[]);
    Ok(match filters.first().map(Vec::as_slice) {
        None => at.checked_add(raw_length(dict, cs, limits)?).filter(|e| *e <= data.len()).ok_or(PressError::Malformed("inline_image_length"))?,
        Some(f) if crate::raster::is_raster_filter(f) => {
            // The first EI before which the data has the codec's whole structure.
            let codec = String::from_utf8_lossy(f).into_owned();
            let mut from = 0usize;
            loop {
                let Some(p) = crate::reader::find(rest.get(from..).unwrap_or(&[]), b"EI") else {
                    return Err(PressError::Malformed("inline_image_end"));
                };
                let q = from.saturating_add(p);
                let mut body = rest.get(..q).unwrap_or(&[]);
                while body.last().is_some_and(|b| is_ws(*b)) {
                    body = body.get(..body.len().saturating_sub(1)).unwrap_or(&[]);
                }
                if ei_at(rest, q) && crate::raster::codec_signature(&codec, body, dict).is_ok() {
                    break at.saturating_add(body.len());
                }
                from = q.saturating_add(1);
            }
        }
        Some(_) => at.checked_add(crate::filters::first_stage_extent(dict, rest, limits, budget)?).filter(|e| *e <= data.len()).ok_or(PressError::Malformed("inline_image_length"))?,
    })
}

/// Operand types of the operators with a fixed operand list.
#[derive(Clone, Copy)]
enum Ty {
    Num,
    Name,
    Str,
    /// `TJ`: strings and numbers.
    TextArray,
    /// `d`: numbers.
    NumArray,
    /// `BDC` / `DP`: a property list, inline or by resource name.
    Props,
}

use Ty::{Name as N, Num as F, Props as P, Str as S};

/// Operand list of an ISO 32000-2 Annex A operator (`None`: not an operator; `SC`/`sc`/`SCN`/`scn` are
/// checked separately).
fn signature(name: &[u8]) -> Option<&'static [Ty]> {
    Some(match name {
        b"q" | b"Q" | b"h" | b"S" | b"s" | b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*" | b"n" | b"W" | b"W*" | b"BT" | b"ET" | b"T*"
        | b"BX" | b"EX" | b"EMC" => &[],
        b"w" | b"J" | b"j" | b"M" | b"i" | b"Tc" | b"Tw" | b"Tz" | b"TL" | b"Tr" | b"Ts" | b"G" | b"g" => &[F],
        b"ri" | b"gs" | b"CS" | b"cs" | b"sh" | b"Do" | b"MP" | b"BMC" => &[N],
        b"m" | b"l" | b"d0" | b"Td" | b"TD" => &[F, F],
        b"d" => &[Ty::NumArray, F],
        b"RG" | b"rg" => &[F, F, F],
        b"v" | b"y" | b"re" | b"K" | b"k" => &[F, F, F, F],
        b"cm" | b"c" | b"d1" | b"Tm" => &[F, F, F, F, F, F],
        b"Tf" => &[N, F],
        b"Tj" | b"'" => &[S],
        b"TJ" => &[Ty::TextArray],
        b"\"" => &[F, F, S],
        b"DP" | b"BDC" => &[N, P],
        _ => return None,
    })
}

fn is_num(o: &Object) -> bool {
    matches!(o, Object::Int(_) | Object::Real(_))
}

fn fits(ty: Ty, o: &Object) -> bool {
    match ty {
        Ty::Num => is_num(o),
        Ty::Name => matches!(o, Object::Name(_)),
        Ty::Str => matches!(o, Object::Str(_)),
        Ty::TextArray => matches!(o, Object::Array(a) if a.iter().all(|x| is_num(x) || matches!(x, Object::Str(_)))),
        Ty::NumArray => matches!(o, Object::Array(a) if a.iter().all(is_num)),
        Ty::Props => matches!(o, Object::Name(_) | Object::Dict(_)),
    }
}

/// The operator is an ISO 32000-2 Annex A operator with exactly its operands (module docs; R10-F5).
pub(crate) fn check_op(op: &Op) -> Result<()> {
    let ops = &op.operands;
    match op.name.as_slice() {
        b"BI" => {
            if !ops.len().is_multiple_of(2) || ops.chunks(2).any(|p| !matches!(p.first(), Some(Object::Name(_)))) {
                return Err(PressError::Malformed("inline_image_dict"));
            }
            Ok(())
        }
        // Colour components: 1 to 4 numbers (ISO 32000-2 8.6.8; pdf.js accepts up to 4).
        b"SC" | b"sc" => {
            if (1..=4).contains(&ops.len()) && ops.iter().all(is_num) {
                Ok(())
            } else {
                Err(PressError::Malformed("operand_count"))
            }
        }
        // Up to 32 components (DeviceN) and an optional pattern name last.
        b"SCN" | b"scn" => {
            let (last, head) = ops.split_last().ok_or(PressError::Malformed("operand_count"))?;
            let comps = if matches!(last, Object::Name(_)) { head } else { ops.as_slice() };
            if comps.len() <= 32 && comps.iter().all(is_num) {
                Ok(())
            } else {
                Err(PressError::Malformed("operand_count"))
            }
        }
        name => {
            let sig = signature(name).ok_or(PressError::Malformed("unknown_operator"))?;
            if sig.len() != ops.len() {
                return Err(PressError::Malformed("operand_count"));
            }
            if sig.iter().zip(ops).all(|(t, o)| fits(*t, o)) { Ok(()) } else { Err(PressError::Malformed("operand_type")) }
        }
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    fn strict(s: &[u8]) -> Result<Parsed> {
        let mut budget = usize::MAX;
        parse_strict(s, &Limits::default(), &no_color_spaces, &mut budget)
    }

    #[test]
    fn operand_counts_and_types_are_exact() {
        assert!(strict(b"BT /F1 12 Tf 1 0 0 1 5 5 Tm (A) Tj [(B) -5 (C)] TJ 1 2 (D) \" ET").is_ok());
        for bad in ["(A) (B) Tj", "[(A)] [(B)] TJ", "/F9 /F1 12 Tf", "/Fm0 /Fm1 Do", "/G0 /G1 gs", "12 /F1 Tf", "(A) foo", "Tj", "1 2 3 cm", "[(A) /x] TJ", "true g"] {
            assert!(strict(bad.as_bytes()).is_err(), "{bad}");
        }
        assert!(strict(b"0.5 sc 1 0 0 sc /P1 scn 0.2 0.3 /P2 scn 0 0 0 0 0 sc").is_err(), "five components");
        assert!(strict(b"0.5 sc 1 0 0 sc /P1 scn 0.2 0.3 /P2 scn").is_ok());
        assert!(strict(b"/Span << /ActualText (x) >> BDC EMC /Tag /Props DP").is_ok());
    }

    #[test]
    fn inline_images_end_where_their_data_ends() {
        // pdf.js accepts EI right after the data (issue10388): the 1-byte image is `A`.
        let src: &[u8] = b"q BI /W 1 /H 1 /BPC 8 /CS /G ID AEI Q BT /F1 12 Tf (S) Tj ET % EI\n";
        let p = strict(src).unwrap();
        assert_eq!(p.ops.iter().map(|o| o.name.as_slice()).collect::<Vec<_>>(), [&b"q"[..], b"BI", b"Q", b"BT", b"Tf", b"Tj", b"ET"]);
        let (a, b) = p.ops[1].inline_data.unwrap();
        assert_eq!(&src[a..b], b"A");
        // A stray EI later is an unknown operator; data longer than the image is a refusal.
        assert!(strict(b"BI /W 1 /H 1 /BPC 8 /CS /G ID AEI Q EI").is_err());
        assert!(strict(b"BI /W 1 /H 1 /BPC 8 /CS /G ID AB EI").is_err());
        // Two rows of 3 bits padded to bytes; an image mask; a named colour space needs resources.
        assert!(strict(b"BI /W 3 /H 2 /BPC 1 /CS /G ID \x00\x00 EI").is_ok());
        assert!(strict(b"BI /IM true /W 9 /H 1 ID \x00\x00\nEI").is_ok());
        assert!(strict(b"BI /W 1 /H 1 /BPC 8 /CS /CS0 ID A EI").is_err());
        let mut budget = usize::MAX;
        let cs = |n: &[u8]| (n == b"CS0").then_some(3);
        assert!(parse_strict(b"BI /W 1 /H 1 /BPC 8 /CS /CS0 ID ABC EI", &Limits::default(), &cs, &mut budget).is_ok());
        // Filtered data ends at the filter's end-of-data marker.
        let src: &[u8] = b"BI /W 1 /H 1 /BPC 8 /CS /G /F /AHx ID 41> EI Q";
        let p = strict(src).unwrap();
        let (a, b) = p.ops[0].inline_data.unwrap();
        assert_eq!(&src[a..b], b"41>");
        assert!(strict(b"BI /W 1 /H 1 /BPC 8 /CS /G /F /AHx ID 41> (S) Tj EI").is_err());
        // Operands before BI, no white space after ID.
        assert!(strict(b"(S) BI /W 1 /H 1 /BPC 8 /CS /G ID A EI").is_err());
        assert!(strict(b"BI /W 1 /H 1 /BPC 8 /CS /G ID<A EI").is_err());
        // Data holding `EI ` is reported for pdf.js's heuristic.
        let p = strict(b"BI /W 4 /H 1 /BPC 8 /CS /G ID EI x EI").unwrap();
        assert!(p.inline_ambiguous);
        // dims_u32_max: dimensions that overflow are refused, not wrapped.
        assert!(strict(b"BI /W 4294967295 /H 4294967295 /BPC 16 /CS /CMYK ID A EI").is_err());
    }

    #[test]
    fn segments_join_only_at_clean_boundaries() {
        let p = |s: &[u8]| {
            let mut budget = usize::MAX;
            tokenize(s, &Limits::default(), &no_color_spaces, &mut budget).unwrap()
        };
        assert!(!joins_cleanly(b"q", &p(b"q"), b"Q"));
        assert!(joins_cleanly(b"q\n", &p(b"q\n"), b"Q"));
        assert!(joins_cleanly(b"(a) Tj", &p(b"(a) Tj"), b" Q"));
        assert!(!joins_cleanly(b"q % note", &p(b"q % note"), b"\nQ"));
        assert!(joins_cleanly(b"q % note\n", &p(b"q % note\n"), b"Q"));
    }

    /// V4-02: finding where a Flate inline image ends inflates it; the output is charged to the
    /// operation budget (aggregate_over_budget), and an overrun is a refusal, not a guessed end.
    #[test]
    fn inline_image_extents_are_charged_to_the_budget() {
        let mut src = b"BI /W 50000 /H 1 /BPC 8 /CS /G /F /Fl ID ".to_vec();
        src.extend_from_slice(&miniz_oxide::deflate::compress_to_vec_zlib(&[0u8; 50_000], 6));
        src.extend_from_slice(b" EI");
        let mut budget = 10_000usize;
        assert_eq!(parse_strict(&src, &Limits::default(), &no_color_spaces, &mut budget).unwrap_err(), PressError::InputLimit("decoded_total"));
        let mut budget = 10_000usize;
        assert_eq!(tokenize(&src, &Limits::default(), &no_color_spaces, &mut budget).unwrap_err(), PressError::InputLimit("decoded_total"), "the lenient reading does not guess either");
        let mut budget = usize::MAX;
        let p = parse_strict(&src, &Limits::default(), &no_color_spaces, &mut budget).unwrap();
        assert_eq!(usize::MAX - budget, 50_000);
        assert_eq!(p.ops.len(), 1);
    }
}
