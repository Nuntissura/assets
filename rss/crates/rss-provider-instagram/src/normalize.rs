use std::collections::BTreeSet;
use chrono::{DateTime, Utc};
use rss_contracts::{AccessMode, Bounds, Error, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use crate::{EntryEvidence, MediaEvidence, invalid};

pub struct Account { pub id: String, pub username: String, pub media_count: Option<u64> }
pub struct Document { pub entries: Vec<EntryEvidence>, pub next: Option<String>, pub digest: String }

pub fn parse_json(bytes: &[u8], max_bytes: usize) -> Result<Value> {
    if bytes.len() > max_bytes { return Err(Error::LimitExceeded { field: "instagram.body".into() }); }
    handshake_canonical_json::jcs_from_slice(bytes, max_bytes).map_err(|_| invalid("instagram.json"))?;
    let value: Value = serde_json::from_slice(bytes).map_err(|_| invalid("instagram.json"))?;
    if value.get("challenge").is_some() || value.get("checkpoint_url").is_some() { return Err(Error::Challenged); }
    if let Some(message) = value.get("message").and_then(Value::as_str) {
        match message {
            "login_required" => return Err(Error::ReconnectRequired),
            "challenge_required" | "checkpoint_required" => return Err(Error::Challenged),
            "feedback_required" | "Please wait a few minutes before you try again." => return Err(Error::RateLimited { retry_after: None }),
            "session_expired" => return Err(Error::Expired),
            _ => {}
        }
    }
    if value.get("status").and_then(Value::as_str) != Some("ok") { return Err(invalid("instagram.status")); }
    Ok(value)
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> { value.get(key).and_then(Value::as_str).ok_or_else(|| invalid(key)) }
fn id(value: &Value) -> Result<String> {
    let result = if let Some(value) = value.as_str() { value.to_owned() }
        else if let Some(value) = value.as_u64() { value.to_string() } else { return Err(invalid("instagram.numeric_id")); };
    if !crate::public::numeric_id(&result) { return Err(invalid("instagram.numeric_id")); } Ok(result)
}
fn user_id(value: &Value) -> Result<String> {
    let first = value.get("pk").or_else(|| value.get("id")).ok_or_else(|| invalid("instagram.user_id"))?;
    let result = id(first)?;
    if let Some(other) = value.get("id") { if id(other)? != result { return Err(invalid("instagram.user_id_conflict")); } }
    Ok(result)
}
pub fn account(bytes: &[u8], username: &str, max_bytes: usize, bounds: &Bounds) -> Result<Account> {
    account_with_access(bytes, username, max_bytes, bounds, None)
}
pub(crate) fn connected_account(bytes: &[u8], username: &str, max_bytes: usize, bounds: &Bounds, mode: AccessMode, login: &str) -> Result<Account> {
    account_with_access(bytes, username, max_bytes, bounds, Some((mode, login)))
}
fn account_with_access(bytes: &[u8], username: &str, max_bytes: usize, bounds: &Bounds, access: Option<(AccessMode, &str)>) -> Result<Account> {
    let root = parse_json(bytes, max_bytes)?;
    let user = &root["data"]["user"];
    let private = user.get("is_private").and_then(Value::as_bool).ok_or(Error::Denied)?;
    if access.is_none() && private { return Err(Error::Denied); }
    let target = user_id(user)?;
    match access {
        Some((AccessMode::OwnUploads, login)) if target == login => {},
        Some((AccessMode::PrivateSource, login)) if private && (target == login || user.get("followed_by_viewer").and_then(Value::as_bool) == Some(true)) => {},
        Some(_) => return Err(Error::Denied),
        None => {},
    }
    let actual = text(user, "username")?;
    if !actual.eq_ignore_ascii_case(username) { return Err(Error::StaleResult); }
    let account = Account { id: target, username: actual.to_owned(), media_count: user.get("media_count").and_then(Value::as_u64) };
    bounds.text("instagram.account", &account.id, bounds.max_text_bytes)?;
    Ok(account)
}
pub fn document(bytes: &[u8], mode: AccessMode, account: &Account, max_bytes: usize, max_entries: usize, bounds: &Bounds) -> Result<Document> {
    let root = parse_json(bytes, max_bytes)?;
    let items = root.get("items").and_then(Value::as_array).ok_or_else(|| invalid("instagram.items"))?;
    if items.len() > max_entries { return Err(Error::LimitExceeded { field: "instagram.items".into() }); }
    let pagination = if mode == AccessMode::PublicReels { &root["paging_info"] } else { &root };
    let more = pagination.get("more_available").and_then(Value::as_bool).ok_or_else(|| invalid("instagram.more_available"))?;
    let field = if mode == AccessMode::PublicReels { "max_id" } else { "next_max_id" };
    let cursor = pagination.get(field).filter(|value| !value.is_null());
    let next = if more {
        let value = cursor.and_then(Value::as_str).filter(|v| !v.is_empty()).ok_or_else(|| invalid("instagram.next_cursor"))?;
        bounds.text("instagram.next_cursor", value, bounds.max_text_bytes)?;
        if value.chars().any(char::is_control) { return Err(invalid("instagram.next_cursor")); }
        Some(value.to_owned())
    } else {
        if cursor.is_some_and(|v| v.as_str() != Some("")) { return Err(invalid("instagram.terminal_cursor")); }
        None
    };
    if items.is_empty() && more { return Err(invalid("instagram.empty_continues")); }
    let digest = format!("{:x}", Sha256::digest(bytes));
    let mut entries: Vec<EntryEvidence> = vec![];
    for item in items {
        let item = if mode == AccessMode::PublicReels { item.get("media").ok_or_else(|| invalid("instagram.clip_media"))? } else { item };
        let entry = entry(item, account, &digest, bounds)?;
        if let Some(prior) = entries.iter().find(|prior| prior.key == entry.key) {
            if prior != &entry { return Err(invalid("instagram.repeated_identity")); }
        } else { entries.push(entry); }
    }
    Ok(Document { entries, next, digest })
}
fn entry(item: &Value, account: &Account, digest: &str, bounds: &Bounds) -> Result<EntryEvidence> {
    let key = id(item.get("pk").ok_or_else(|| invalid("instagram.media_id"))?)?;
    let code = text(item, "code")?.to_owned();
    let owner_id = user_id(&item["user"])?;
    let author = text(&item["user"], "username")?.to_owned();
    if owner_id == account.id && !author.eq_ignore_ascii_case(&account.username) { return Err(invalid("instagram.owner_identity")); }
    let collaborators = item.get("coauthor_producers").filter(|v| !v.is_null()).map(|v| v.as_array().ok_or_else(|| invalid("instagram.coauthors"))).transpose()?;
    if let Some(coauthors) = collaborators {
        if coauthors.len() > bounds.max_collection_items { return Err(invalid("instagram.coauthors")); }
        for coauthor in coauthors {
            let coauthor_id = user_id(coauthor)?;
            let name = text(coauthor,"username")?;
            if !crate::public::username(name) { return Err(invalid("instagram.coauthor")); }
            if coauthor_id == account.id {
                if !name.eq_ignore_ascii_case(&account.username) { return Err(invalid("instagram.coauthor_identity")); }
            }
        }
    }
    // Membership comes from the verified account endpoint, not equality with the primary post author.
    if !crate::public::shortcode(&code) { return Err(invalid("instagram.shortcode")); }
    if let Some(value) = item.get("id") {
        let expected = format!("{key}_{owner_id}");
        if value.as_str() != Some(key.as_str()) && value.as_str() != Some(expected.as_str()) { return Err(invalid("instagram.media_id_conflict")); }
    }
    let description = match item.get("caption") {
        None | Some(Value::Null) => None,
        Some(caption) => Some(text(caption,"text")?.to_owned()),
    };
    let published_at = item.get("taken_at").filter(|v| !v.is_null()).map(|value| -> Result<_> {
        let seconds = value.as_i64().filter(|n| *n >= 0).ok_or_else(|| invalid("instagram.taken_at"))?;
        let date = DateTime::<Utc>::from_timestamp(seconds,0).ok_or_else(|| invalid("instagram.taken_at"))?;
        Ok(hsscrap::PublicationTime { raw_value: seconds.to_string(), precision: hsscrap::PublicationPrecision::Second,
            local_value: date.format("%Y-%m-%dT%H:%M:%S").to_string(), offset_minutes: Some(0) })
    }).transpose()?;
    let kind = item.get("media_type").and_then(Value::as_u64).ok_or_else(|| invalid("instagram.media_type"))?;
    let members: Vec<&Value> = if kind == 8 {
        let members = item.get("carousel_media").and_then(Value::as_array).ok_or_else(|| invalid("instagram.carousel"))?;
        if let Some(count) = item.get("carousel_media_count") { if count.as_u64() != Some(members.len() as u64) { return Err(invalid("instagram.carousel_count")); } }
        members.iter().collect()
    } else {
        if item.get("carousel_media").is_some_and(|v| !v.is_null()) { return Err(invalid("instagram.unexpected_carousel")); }
        vec![item]
    };
    if members.is_empty() || members.len() > bounds.max_collection_items { return Err(invalid("instagram.carousel")); }
    let mut media = vec![]; let mut ids = BTreeSet::new();
    for member in members {
        let member_id = id(member.get("pk").ok_or_else(|| invalid("instagram.member_id"))?)?;
        if !ids.insert(member_id.clone()) { return Err(invalid("instagram.duplicate_member")); }
        media.push(media_member(member, member_id, bounds)?);
    }
    let evidence = EntryEvidence { key, account_id: account.id.clone(), owner_id, url: format!("https://www.instagram.com/p/{code}/"), shortcode: code,
        author, description, published_at, media, document_digest: digest.to_owned() };
    evidence.validate(bounds)?;
    Ok(evidence)
}
fn representation(values: &Value, bounds: &Bounds) -> Result<(String,u32,u32)> {
    let values = values.as_array().filter(|v| !v.is_empty()).ok_or_else(|| invalid("instagram.representations"))?;
    if values.len() > bounds.max_collection_items { return Err(Error::LimitExceeded { field: "instagram.representations".into() }); }
    let mut selected = None;
    for value in values {
        let url = text(value,"url")?.to_owned(); crate::public::media_url(&url,bounds)?;
        let width = value.get("width").and_then(Value::as_u64).and_then(|v| u32::try_from(v).ok()).filter(|v| *v > 0).ok_or_else(|| invalid("instagram.width"))?;
        let height = value.get("height").and_then(Value::as_u64).and_then(|v| u32::try_from(v).ok()).filter(|v| *v > 0).ok_or_else(|| invalid("instagram.height"))?;
        let candidate = (url,width,height);
        if selected.as_ref().is_none_or(|prior: &(String,u32,u32)| (width,height) > (prior.1,prior.2)) { selected = Some(candidate); }
    }
    selected.ok_or_else(|| invalid("instagram.representations"))
}
fn media_member(value: &Value, provider_media_id: String, bounds: &Bounds) -> Result<MediaEvidence> {
    let kind = value.get("media_type").and_then(Value::as_u64).ok_or_else(|| invalid("instagram.member_type"))?;
    let (image,iw,ih) = representation(&value["image_versions2"]["candidates"],bounds)?;
    let (kind,url,width,height,preview_url) = match kind {
        1 => (hsscrap::MediaKind::Image,image,iw,ih,None),
        2 => { let (url,w,h) = representation(&value["video_versions"],bounds)?; (hsscrap::MediaKind::Video,url,w,h,Some(image)) },
        _ => return Err(invalid("instagram.member_type")),
    };
    let duration_ms = value.get("video_duration").filter(|v| !v.is_null()).map(|v| {
        let seconds = v.as_f64().filter(|v| v.is_finite() && *v >= 0.0 && *v <= (u64::MAX / 1000) as f64).ok_or_else(|| invalid("instagram.duration"))?;
        Ok::<u64,Error>((seconds * 1000.0).round() as u64)
    }).transpose()?;
    let expires_at_unix = value.get("expiring_at").filter(|v| !v.is_null()).map(|v| v.as_u64().ok_or_else(|| invalid("instagram.expiry"))).transpose()?;
    Ok(MediaEvidence { provider_media_id,kind,url,preview_url,width,height,duration_ms,expires_at_unix })
}
