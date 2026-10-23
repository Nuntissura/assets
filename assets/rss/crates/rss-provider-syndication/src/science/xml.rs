use crate::invalid;
use quick_xml::{events::Event, name::ResolveResult, NsReader, XmlVersion};
use rss_contracts::{Bounds, Result};
use std::collections::BTreeMap;
const ATOM: &str = "http://www.w3.org/2005/Atom";
const OPENSEARCH: &str = "http://a9.com/-/spec/opensearch/1.1/";
const ARXIV: &str = "http://arxiv.org/schemas/atom";
#[derive(Default)]
pub(super) struct RawEntry {
    pub id: String,
    pub fields: BTreeMap<String, String>,
    pub links: Vec<String>,
}
pub(super) struct RawDocument {
    pub entries: Vec<RawEntry>,
    pub counters: BTreeMap<String, u64>,
}
struct Frame {
    namespace: String,
    name: String,
    text: String,
    attributes: BTreeMap<String, String>,
}
fn insert(map: &mut BTreeMap<String, String>, key: &str, value: String) -> Result<()> {
    if map.insert(key.into(), value).is_some() {
        return Err(invalid("science.duplicate_field"));
    }
    Ok(())
}
pub(super) fn parse(bytes: &[u8], max_entries: usize, bounds: &Bounds) -> Result<RawDocument> {
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("science.xml_encoding"))?;
    let mut reader = NsReader::from_str(text);
    let mut version = XmlVersion::Implicit1_0;
    let mut stack: Vec<Frame> = vec![];
    let mut entries = vec![];
    let mut current: Option<(usize, RawEntry)> = None;
    let mut counters = BTreeMap::new();
    loop {
        let (namespace, event) = reader
            .read_resolved_event()
            .map_err(|_| invalid("science.xml"))?;
        let namespace = match namespace {
            ResolveResult::Bound(value) => value.as_ref().to_owned(),
            ResolveResult::Unbound => String::new(),
            ResolveResult::Unknown(_) => return Err(invalid("science.namespace")),
        };
        let empty = matches!(&event, Event::Empty(_));
        match event {
            Event::Start(value) | Event::Empty(value) => {
                if stack.len() >= 64 {
                    return Err(invalid("science.xml_depth"));
                }
                let name = value.local_name().as_ref().to_owned();
                let mut attributes = BTreeMap::new();
                for attribute in value.attributes() {
                    let attribute = attribute.map_err(|_| invalid("science.xml_attribute"))?;
                    if attributes.len() >= bounds.max_collection_items {
                        return Err(invalid("science.xml_attributes"));
                    }
                    let key = attribute.key.as_ref().to_owned();
                    let value = attribute
                        .normalized_value(version)
                        .map_err(|_| invalid("science.xml_attribute"))?
                        .into_owned();
                    bounds.text("science.xml_attribute", &value, bounds.max_text_bytes)?;
                    if attributes.insert(key, value).is_some() {
                        return Err(invalid("science.xml_attribute"));
                    }
                }
                if (namespace == ATOM && name == "entry")
                    || (namespace.is_empty() && name == "item")
                {
                    if current.is_some() || entries.len() >= max_entries {
                        return Err(invalid("science.entries"));
                    }
                    current = Some((stack.len(), RawEntry::default()));
                }
                stack.push(Frame {
                    namespace,
                    name,
                    text: String::new(),
                    attributes,
                });
                if empty {
                    close(
                        &mut stack,
                        &mut current,
                        &mut entries,
                        &mut counters,
                        bounds,
                    )?;
                }
            }
            Event::End(_) => close(
                &mut stack,
                &mut current,
                &mut entries,
                &mut counters,
                bounds,
            )?,
            Event::Text(value) => append(&mut stack, value.as_ref(), bounds)?,
            Event::CData(value) => append(&mut stack, value.as_ref(), bounds)?,
            Event::GeneralRef(value) => {
                let escaped = format!("&{};", value.as_ref());
                let decoded = quick_xml::escape::unescape(&escaped)
                    .map_err(|_| invalid("science.xml_entity"))?;
                append(&mut stack, &decoded, bounds)?;
            }
            Event::DocType(_) => return Err(invalid("science.xml_doctype")),
            Event::Decl(value) => {
                version = value
                    .xml_version()
                    .map_err(|_| invalid("science.xml_version"))?
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !stack.is_empty() || current.is_some() {
        return Err(invalid("science.xml_unclosed"));
    }
    Ok(RawDocument { entries, counters })
}
fn append(stack: &mut [Frame], value: &str, bounds: &Bounds) -> Result<()> {
    if let Some(frame) = stack.last_mut() {
        frame.text.push_str(value);
        bounds.text("science.xml_text", &frame.text, bounds.max_text_bytes)?;
    }
    Ok(())
}
fn close(
    stack: &mut Vec<Frame>,
    current: &mut Option<(usize, RawEntry)>,
    entries: &mut Vec<RawEntry>,
    counters: &mut BTreeMap<String, u64>,
    bounds: &Bounds,
) -> Result<()> {
    let frame = stack.pop().ok_or_else(|| invalid("science.xml_end"))?;
    let text = frame.text.trim().to_owned();
    if let Some((depth, entry)) = current {
        if stack.len() == *depth + 1 {
            if (frame.namespace == ATOM && frame.name == "id")
                || (frame.namespace.is_empty() && frame.name == "guid")
            {
                if !entry.id.is_empty() || text.is_empty() {
                    return Err(invalid("science.entry_id"));
                }
                entry.id = text;
            } else if frame.namespace == ATOM && frame.name == "updated" {
                insert(&mut entry.fields, "updated", text)?;
            } else if frame.namespace == ARXIV
                && matches!(
                    frame.name.as_str(),
                    "doi"
                        | "DOI"
                        | "journal_ref"
                        | "journal_reference"
                        | "comment"
                        | "announce_type"
                )
            {
                insert(&mut entry.fields, &frame.name, text)?;
            } else if frame.namespace == ATOM && frame.name == "link" {
                if let Some(url) = frame.attributes.get("href") {
                    if entry.links.len() >= bounds.max_collection_items {
                        return Err(invalid("science.links"));
                    }
                    entry.links.push(url.clone());
                }
            } else if frame.namespace.is_empty() && frame.name == "link" && !text.is_empty() {
                entry.links.push(text);
            }
        }
        if stack.len() == *depth {
            let (_, entry) = current.take().ok_or_else(|| invalid("science.entry"))?;
            if entry.id.is_empty() {
                return Err(invalid("science.entry_id"));
            }
            entries.push(entry);
        }
    } else if frame.namespace == OPENSEARCH
        && matches!(
            frame.name.as_str(),
            "totalResults" | "startIndex" | "itemsPerPage"
        )
    {
        if stack.len() != 1
            || counters
                .insert(
                    frame.name,
                    text.parse().map_err(|_| invalid("science.api_counter"))?,
                )
                .is_some()
        {
            return Err(invalid("science.api_counter"));
        }
    }
    Ok(())
}
