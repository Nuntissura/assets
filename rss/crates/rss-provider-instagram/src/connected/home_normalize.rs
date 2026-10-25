use super::home::HomeDocumentKind;
use crate::{invalid, normalize, EntryEvidence};
use rss_contracts::{Bounds, Error, Result};
use scraper::{Html, Selector};
use serde_json::Value;
use sha2::{Digest, Sha256};

const ROOT: &str = "xdt_api__v1__feed__timeline__connection";

fn json(bytes: &[u8], max_bytes: usize) -> Result<Value> {
    handshake_canonical_json::jcs_from_slice(bytes, max_bytes).map_err(|_| invalid("instagram.home_json"))?;
    serde_json::from_slice(bytes).map_err(|_| invalid("instagram.home_json"))
}

fn errors(root: &Value) -> Result<()> {
    normalize::response_errors(root)?;
    if root.get("message").is_some() || root.get("status").is_some_and(|status| status.as_str() != Some("ok")) {
        return Err(invalid("instagram.home_status"));
    }
    if root.get("errors").is_some_and(|errors| !errors.is_null() && errors.as_array().is_none_or(|items| !items.is_empty())) {
        return Err(invalid("instagram.home_graphql_errors"));
    }
    Ok(())
}

fn find_connection<'a>(root: &'a Value, remaining: &mut usize) -> Result<Option<&'a Value>> {
    let mut pending = vec![(root, None::<Error>)];
    let mut found = None;
    while let Some((value, inherited_error)) = pending.pop() {
        *remaining = remaining.checked_sub(1).ok_or_else(|| invalid("instagram.home_nodes"))?;
        match value {
            Value::Array(items) => {
                if items.len().saturating_add(pending.len()) > *remaining { return Err(invalid("instagram.home_nodes")); }
                pending.extend(items.iter().map(|child| (child, inherited_error.clone())));
            },
            Value::Object(items) => {
                if items.len().saturating_add(pending.len()) > *remaining { return Err(invalid("instagram.home_nodes")); }
                let path_error = inherited_error.or_else(|| errors(value).err());
                for (key, child) in items {
                    if key == ROOT {
                        if let Some(error) = &path_error { return Err(error.clone()); }
                        if !child.is_object() || found.replace(child).is_some() { return Err(invalid("instagram.home_connection_conflict")); }
                    } else { pending.push((child, path_error.clone())); }
                }
            },
            _ => {},
        }
    }
    Ok(found)
}

fn html_connection(bytes: &[u8], max_bytes: usize, max_entries: usize, bounds: &Bounds) -> Result<Value> {
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("instagram.home_html"))?;
    let document = Html::parse_document(text);
    let selector = Selector::parse("script[type=\"application/json\"]").map_err(|_| invalid("instagram.home_html"))?;
    let mut remaining = max_entries.saturating_mul(bounds.max_collection_items).min(max_bytes);
    let mut found = None;
    for (index, script) in document.select(&selector).enumerate() {
        if index >= bounds.max_collection_items { return Err(invalid("instagram.home_scripts")); }
        let mut raw = String::new();
        for chunk in script.text() {
            if raw.len().saturating_add(chunk.len()) > max_bytes { return Err(invalid("instagram.home_script_bytes")); }
            raw.push_str(chunk);
        }
        if raw.trim().is_empty() { continue; }
        let root = json(raw.as_bytes(), max_bytes)?;
        if let Some(connection) = find_connection(&root, &mut remaining)? {
            if found.replace(connection.clone()).is_some() { return Err(invalid("instagram.home_connection_conflict")); }
        }
    }
    found.ok_or_else(|| invalid("instagram.home_connection_missing"))
}

pub(super) fn document(bytes: &[u8], kind: HomeDocumentKind, max_bytes: usize, max_entries: usize, bounds: &Bounds)
    -> Result<normalize::Document> {
    if bytes.len() > max_bytes { return Err(Error::LimitExceeded { field: "instagram.home_body".into() }); }
    let connection = match kind {
        HomeDocumentKind::Html => html_connection(bytes, max_bytes, max_entries, bounds)?,
        HomeDocumentKind::Graphql => {
            let root = json(bytes, max_bytes)?;
            errors(&root)?;
            root.get("data").and_then(|data| data.get(ROOT)).filter(|value| value.is_object())
                .ok_or_else(|| invalid("instagram.home_connection_missing"))?.clone()
        },
    };
    errors(&connection)?;
    let edges = connection.get("edges").and_then(Value::as_array).ok_or_else(|| invalid("instagram.home_edges"))?;
    if edges.len() > max_entries { return Err(Error::LimitExceeded { field: "instagram.home_edges".into() }); }
    let info = connection.get("page_info").filter(|value| value.is_object()).ok_or_else(|| invalid("instagram.home_page_info"))?;
    let more = info.get("has_next_page").and_then(Value::as_bool).ok_or_else(|| invalid("instagram.home_has_next_page"))?;
    let cursor = info.get("end_cursor").ok_or_else(|| invalid("instagram.home_end_cursor"))?;
    let cursor = if cursor.is_null() { None } else {
        let cursor = cursor.as_str().filter(|value| !value.is_empty()).ok_or_else(|| invalid("instagram.home_end_cursor"))?;
        bounds.text("instagram.home_end_cursor", cursor, bounds.max_text_bytes)?;
        if cursor.chars().any(char::is_control) { return Err(invalid("instagram.home_end_cursor")); }
        Some(cursor.to_owned())
    };
    let next = if more { Some(cursor.ok_or_else(|| invalid("instagram.home_end_cursor"))?) } else { None };
    if edges.is_empty() && more { return Err(invalid("instagram.home_empty_continues")); }
    let digest = format!("{:x}", Sha256::digest(bytes));
    let mut entries: Vec<EntryEvidence> = vec![];
    for edge in edges {
        let node = edge.get("node").filter(|value| value.is_object()).ok_or_else(|| invalid("instagram.home_node"))?;
        let media = node.get("media").filter(|value| !value.is_null());
        let story = node.get("explore_story").filter(|value| !value.is_null());
        let media = match (media, story) {
            (Some(media), None) => media,
            (None, Some(story)) => story.get("media").unwrap_or(story),
            _ => return Err(invalid("instagram.home_media")),
        };
        let entry = normalize::timeline_entry(media, &digest, bounds)?;
        if let Some(prior) = entries.iter().find(|prior| prior.key == entry.key) {
            if prior != &entry { return Err(invalid("instagram.home_repeated_identity")); }
        } else { entries.push(entry); }
    }
    Ok(normalize::Document { entries, next, digest })
}
