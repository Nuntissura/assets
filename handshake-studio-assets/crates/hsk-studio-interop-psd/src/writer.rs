//! Re-emission. An unmodified [`PsdDocument`] re-emits byte-identically: raw sub-structures,
//! padding and unknown blocks are written back as read. The only value regenerated from a model
//! field is a layer name that was changed (`PsdLayer::name` differs from what the file states).

use std::borrow::Cow;

use crate::document::{LayerInfo, LayerInfoLocation, LayerSection, PsdDocument};
use crate::error::{PsdError, Result};
use crate::layers::{self, KEY_UNICODE_NAME, PsdLayer, TaggedBlock};
use crate::limits::Limits;

fn u32_len(n: usize) -> Result<[u8; 4]> {
    Ok(u32::try_from(n).map_err(|_| PsdError::Overflow)?.to_be_bytes())
}

fn put_len(out: &mut Vec<u8>, wide: bool, n: usize) -> Result<()> {
    if wide {
        out.extend_from_slice(&(n as u64).to_be_bytes());
    } else {
        out.extend_from_slice(&u32_len(n)?);
    }
    Ok(())
}

/// Returns the layer unchanged when its name still equals the stored name; otherwise a copy with
/// `luni` replaced (or added) and the Pascal name regenerated as Latin-1 (`?` for other
/// characters, truncated to 255 bytes).
fn with_current_name<'a>(layer: &'a PsdLayer, limits: &Limits) -> Cow<'a, PsdLayer> {
    if layer.name == layer.stored_name(limits) {
        return Cow::Borrowed(layer);
    }
    let mut copy = layer.clone();
    let units: Vec<u16> = layer.name.encode_utf16().take(limits.max_name_units).collect();
    let mut data = (units.len() as u32).to_be_bytes().to_vec();
    for u in &units {
        data.extend_from_slice(&u.to_be_bytes());
    }
    while !data.len().is_multiple_of(4) {
        data.push(0);
    }
    match copy.blocks.iter_mut().find(|b| b.key == KEY_UNICODE_NAME) {
        Some(block) => block.data = data,
        None => copy.blocks.push(TaggedBlock { signature: *b"8BIM", key: KEY_UNICODE_NAME, data, pad: Vec::new() }),
    }
    copy.name_pascal = layer.name.chars().map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?')).take(255).collect();
    let pad = (4 - (1 + copy.name_pascal.len()) % 4) % 4;
    copy.name_pad = vec![0; pad];
    Cow::Owned(copy)
}

fn encode_layer_info(info: &LayerInfo, doc: &PsdDocument, limits: &Limits) -> Result<Vec<u8>> {
    let count = i16::try_from(info.layers.len()).map_err(|_| PsdError::Overflow)?;
    let mut out = (if info.merged_alpha { -count } else { count }).to_be_bytes().to_vec();
    let current: Vec<Cow<'_, PsdLayer>> = info.layers.iter().map(|l| with_current_name(l, limits)).collect();
    for layer in &current {
        layers::encode_record(layer, &doc.header, &mut out)?;
    }
    for layer in &current {
        for channel in &layer.channels {
            out.extend_from_slice(&channel.compression.to_be_bytes());
            out.extend_from_slice(&channel.data);
        }
    }
    out.extend_from_slice(&info.trailing);
    Ok(out)
}

fn encode_layer_section(section: &LayerSection, doc: &PsdDocument, limits: &Limits) -> Result<Vec<u8>> {
    let psb = doc.header.is_psb();
    let mut blocks: Vec<Cow<'_, TaggedBlock>> = section.blocks.iter().map(Cow::Borrowed).collect();
    let mut primary = Vec::new();
    if let Some(info) = &section.layer_info {
        let payload = encode_layer_info(info, doc, limits)?;
        match &info.location {
            LayerInfoLocation::Primary => primary = payload,
            LayerInfoLocation::Tagged { signature, key, insert_at, pad } => {
                let block = TaggedBlock { signature: *signature, key: *key, data: payload, pad: pad.clone() };
                blocks.insert((*insert_at).min(blocks.len()), Cow::Owned(block));
            }
        }
    }
    let mut out = Vec::new();
    put_len(&mut out, psb, primary.len())?;
    out.extend_from_slice(&primary);
    if section.global_mask.is_some() || !blocks.is_empty() {
        let mask = section.global_mask.as_deref().unwrap_or_default();
        out.extend_from_slice(&u32_len(mask.len())?);
        out.extend_from_slice(mask);
        for block in &blocks {
            layers::encode_tagged_block(block, psb, &mut out)?;
        }
    }
    out.extend_from_slice(&section.trailing);
    Ok(out)
}

/// Serializes the document. For a document produced by [`crate::read_psd`] and not modified, the
/// output equals the input bytes.
pub fn write_psd(doc: &PsdDocument, limits: &Limits) -> Result<Vec<u8>> {
    let psb = doc.header.is_psb();
    let mut out = Vec::new();
    doc.header.encode(&mut out);
    out.extend_from_slice(&u32_len(doc.color_mode_data.len())?);
    out.extend_from_slice(&doc.color_mode_data);
    let mut resources = Vec::new();
    doc.resources.encode(&mut resources);
    out.extend_from_slice(&u32_len(resources.len())?);
    out.extend_from_slice(&resources);
    let section = &doc.layer_section;
    let empty = section.layer_info.is_none()
        && section.global_mask.is_none()
        && section.blocks.is_empty()
        && section.trailing.is_empty();
    if empty {
        put_len(&mut out, psb, 0)?;
    } else {
        let body = encode_layer_section(section, doc, limits)?;
        put_len(&mut out, psb, body.len())?;
        out.extend_from_slice(&body);
    }
    if let Some(image) = &doc.image_data {
        out.extend_from_slice(&image.compression.to_be_bytes());
        out.extend_from_slice(&image.data);
    }
    if out.len() > limits.max_input_bytes {
        return Err(PsdError::InputTooLarge);
    }
    Ok(out)
}
