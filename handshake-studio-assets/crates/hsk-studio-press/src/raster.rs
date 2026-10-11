//! Raster streams the payload proof cannot search as text (class `raster_unsearchable`).
//!
//! A stream is in the class ONLY when its dictionary is an image XObject (`/Subtype /Image`), or an
//! inline image, AND its filter chain ENDS in `DCTDecode`, `JPXDecode`, `CCITTFaxDecode` or
//! `JBIG2Decode` while every earlier filter is implemented. Crypt, unknown filters, raster filters in
//! any other position, and every non-image stream with an unimplemented filter stay outside the
//! class and block the proof (`PayloadProof::streams_not_decodable`).
//! The part of the chain that can be decoded (the prefix before the raster codec) is still decoded
//! and scanned as bytes: a needle in a JPEG comment segment is a hit; only a needle drawn as pixels
//! is out of reach, and that is what region removal of the image is for. A codec input that is a
//! complete zlib stream is not pixel data: `hidden_zlib` inflates it and the proof scans the result
//! (test `stray_streams_and_hidden_payloads_are_not_kept_text`).
#![deny(clippy::arithmetic_side_effects)]
use crate::error::{Limits, PressError, Result};
use crate::filters;
use crate::syntax::{Dict, Object, Stream};

/// Class name carried by [`crate::RasterStream`].
pub const RASTER_CLASS: &str = "raster_unsearchable";

pub(crate) fn is_raster_filter(name: &[u8]) -> bool {
    matches!(name, b"DCTDecode" | b"DCT" | b"JPXDecode" | b"CCITTFaxDecode" | b"CCF" | b"JBIG2Decode")
}

/// Filter names of a stream dictionary (`/Filter`; for inline images also the abbreviation `/F`).
/// `None` when the entry is not a name or an array of names.
fn filter_chain(dict: &Dict, inline: bool) -> Option<Vec<Vec<u8>>> {
    let v = dict.get("Filter").or_else(|| if inline { dict.get("F") } else { None });
    match v {
        None | Some(Object::Null) => Some(Vec::new()),
        Some(Object::Name(n)) => Some(vec![n.clone()]),
        Some(Object::Array(a)) => a.iter().map(|o| o.as_name().map(<[u8]>::to_vec)).collect(),
        Some(_) => None,
    }
}

/// Whether `filters` decodes a chain consisting of this one filter (single source of truth).
fn implemented(name: &[u8]) -> bool {
    let mut d = Dict::default();
    d.set("Filter", Object::Name(name.to_vec()));
    filters::is_decodable(&d)
}

/// `Some(raster filter name)` when the chain ends in a raster codec and every earlier filter is
/// implemented.
fn tail_of(chain: &[Vec<u8>]) -> Option<String> {
    let (last, head) = chain.split_last()?;
    (is_raster_filter(last) && head.iter().all(|n| implemented(n))).then(|| String::from_utf8_lossy(last).into_owned())
}

/// Class check for an image XObject stream dictionary.
pub(crate) fn raster_tail(dict: &Dict) -> Option<String> {
    if !dict.get("Subtype").is_some_and(|s| s.is_name("Image")) {
        return None;
    }
    tail_of(&filter_chain(dict, false)?)
}

/// What an inline image's filter chain means for the proof.
pub(crate) enum InlineClass {
    /// No filter, or only implemented filters: its bytes are plain content-stream bytes.
    Searchable,
    /// Ends in a raster codec after implemented filters.
    Raster(String),
    /// Anything else (Crypt, unknown, raster codec not last, malformed entry): blocks the proof.
    Blocking(String),
}

/// Classifies an inline image from the key/value operands recorded for `BI`.
pub(crate) fn classify_inline(operands: &[Object]) -> InlineClass {
    let mut d = Dict::default();
    for pair in operands.chunks(2) {
        if let [Object::Name(k), v] = pair {
            d.set_key(k, v.clone());
        }
    }
    let Some(chain) = filter_chain(&d, true) else {
        return InlineClass::Blocking("filter_entry".into());
    };
    if chain.iter().all(|n| implemented(n)) {
        return InlineClass::Searchable;
    }
    match tail_of(&chain) {
        Some(f) => InlineClass::Raster(f),
        None => {
            let bad = chain.iter().find(|n| !implemented(n)).map(|n| String::from_utf8_lossy(n).into_owned()).unwrap_or_default();
            InlineClass::Blocking(bad)
        }
    }
}

/// Decodes every filter except the final raster codec. The result is the codec's input (a JPEG,
/// JPEG 2000, CCITT or JBIG2 codestream), which is scanned as bytes.
pub(crate) fn decode_prefix(stream: &Stream, limits: &Limits, budget: &mut usize) -> Result<Vec<u8>> {
    let chain = filter_chain(&stream.dict, false).ok_or(PressError::Malformed("filter_entry"))?;
    let head = chain.get(..chain.len().saturating_sub(1)).unwrap_or(&[]);
    let mut dict = stream.dict.clone();
    // An explicit (possibly empty) array so the file-specification key `/F` is not read as a filter.
    dict.set("Filter", Object::Array(head.iter().map(|n| Object::Name(n.clone())).collect()));
    filters::decode_stream(&Stream { dict, data: stream.data.clone() }, limits, budget)
}

/// A raster stream whose codec input is itself a complete zlib stream is not pixel data (JPEG,
/// JPEG 2000, CCITT and JBIG2 streams are never valid zlib streams): returns the inflated bytes so
/// the proof scans them, bounded like any decode (a bomb is `InputLimit`). Without this, zlib bytes
/// of a removed string behind a raster filter name would read as unsearchable pixels.
pub(crate) fn hidden_zlib(codec_input: &[u8], limits: &Limits, budget: &mut usize) -> Result<Option<Vec<u8>>> {
    let cap = limits.max_decoded_bytes.min(*budget);
    match miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(codec_input, cap) {
        Ok(v) => {
            *budget = budget.saturating_sub(v.len());
            Ok(Some(v))
        }
        Err(e) if e.status == miniz_oxide::inflate::TINFLStatus::HasMoreOutput => {
            *budget = budget.saturating_sub(cap);
            Err(PressError::InputLimit(if cap < limits.max_decoded_bytes { "decoded_total" } else { "decoded_bytes" }))
        }
        Err(_) => Ok(None),
    }
}
