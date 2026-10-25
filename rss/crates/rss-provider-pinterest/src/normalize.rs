use crate::{BoardEvidence, EntryEvidence, MediaEvidence, MemberKey, invalid, public};
use rss_contracts::{Bounds, Error, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
pub struct Board {
    pub source: BoardEvidence,
    pub pin_count: Option<u64>,
}
pub struct Section {
    pub id: String,
    pub pin_count: Option<u64>,
}
pub struct Document {
    pub entries: Vec<EntryEvidence>,
    pub next: Option<Vec<String>>,
    pub digest: String,
}
pub fn json(bytes: &[u8], max: usize) -> Result<Value> {
    handshake_canonical_json::jcs_from_slice(bytes, max).map_err(|_| invalid("pinterest.json"))?;
    let root: Value = serde_json::from_slice(bytes).map_err(|_| invalid("pinterest.json"))?;
    let response = root
        .get("resource_response")
        .filter(|v| v.is_object())
        .ok_or_else(|| invalid("pinterest.resource_response"))?;
    if response.get("error").is_some_and(|v| !v.is_null())
        || response
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| status != "success")
    {
        return Err(Error::Denied);
    }
    Ok(root)
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(key))
}
fn optional_text(value: &Value, key: &str) -> Result<Option<String>> {
    value
        .get(key)
        .filter(|v| !v.is_null())
        .map(|v| v.as_str().map(str::to_owned).ok_or_else(|| invalid(key)))
        .transpose()
}
fn id(value: &Value) -> Result<String> {
    let value = if let Some(value) = value.as_str() {
        value.into()
    } else if let Some(value) = value.as_u64() {
        value.to_string()
    } else {
        return Err(invalid("pinterest.numeric_id"));
    };
    if !public::numeric_id(&value) {
        return Err(invalid("pinterest.numeric_id"));
    }
    Ok(value)
}
fn public_board(value: &Value, bounds: &Bounds) -> Result<BoardEvidence> {
    if value.get("is_secret").and_then(Value::as_bool) == Some(true)
        || value
            .get("privacy")
            .and_then(Value::as_str)
            .is_some_and(|v| !v.eq_ignore_ascii_case("public"))
    {
        return Err(Error::Denied);
    }
    if value.get("is_secret").and_then(Value::as_bool) != Some(false)
        && !value
            .get("privacy")
            .and_then(Value::as_str)
            .is_some_and(|v| v.eq_ignore_ascii_case("public"))
    {
        return Err(invalid("pinterest.board_visibility"));
    }
    let id = id(value
        .get("id")
        .ok_or_else(|| invalid("pinterest.board_id"))?)?;
    let owner = text(&value["owner"], "username")?.to_ascii_lowercase();
    let raw = text(value, "url")?;
    let url = if raw.starts_with('/') {
        format!("https://www.pinterest.com{raw}")
    } else {
        raw.into()
    };
    let public::SourceTarget::Board { owner: actual, .. } = public::resolve_url(&url, bounds)?
    else {
        return Err(invalid("pinterest.board_url"));
    };
    if actual != owner {
        return Err(invalid("pinterest.board_owner"));
    }
    let title = optional_text(value, "name")?;
    if let Some(title) = &title {
        bounds.text("pinterest.board_name", title, bounds.max_text_bytes)?;
    }
    Ok(BoardEvidence {
        id,
        owner,
        url: public::canonical_source(&public::resolve_url(&url, bounds)?)?,
        title,
    })
}
pub fn board(bytes: &[u8], owner: &str, slug: &str, max: usize, bounds: &Bounds) -> Result<Board> {
    let root = json(bytes, max)?;
    let data = &root["resource_response"]["data"];
    let source = public_board(data, bounds)?;
    if source.owner != owner
        || public::resolve_url(&source.url, bounds)?
            != (public::SourceTarget::Board {
                owner: owner.into(),
                slug: slug.into(),
            })
    {
        return Err(Error::StaleResult);
    }
    let pin_count = data
        .get("pin_count")
        .filter(|v| !v.is_null())
        .map(|v| v.as_u64().ok_or_else(|| invalid("pinterest.pin_count")))
        .transpose()?;
    Ok(Board { source, pin_count })
}
pub fn section(bytes: &[u8], board: &Board, max: usize) -> Result<Section> {
    let root = json(bytes, max)?;
    let data = &root["resource_response"]["data"];
    if id(data["board"]
        .get("id")
        .ok_or_else(|| invalid("pinterest.section_board"))?)?
        != board.source.id
    {
        return Err(Error::StaleResult);
    }
    let id = id(data
        .get("id")
        .ok_or_else(|| invalid("pinterest.section_id"))?)?;
    let pin_count = data
        .get("pin_count")
        .filter(|v| !v.is_null())
        .map(|v| v.as_u64().ok_or_else(|| invalid("pinterest.pin_count")))
        .transpose()?;
    Ok(Section { id, pin_count })
}
fn values(root: &Value, max_entries: usize) -> Result<&Vec<Value>> {
    let data = &root["resource_response"]["data"];
    let values = data
        .as_array()
        .or_else(|| data.get("results").and_then(Value::as_array))
        .ok_or_else(|| invalid("pinterest.results"))?;
    if values.len() > max_entries {
        return Err(Error::LimitExceeded {
            field: "pinterest.results".into(),
        });
    }
    Ok(values)
}
pub fn bookmarks(root: &Value, bounds: &Bounds) -> Result<Option<Vec<String>>> {
    let values = root["resource"]["options"]["bookmarks"]
        .as_array()
        .ok_or_else(|| invalid("pinterest.bookmarks"))?;
    if values.len() > bounds.max_collection_items {
        return Err(invalid("pinterest.bookmarks"));
    }
    let mut result = vec![];
    for value in values {
        let value = value
            .as_str()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| invalid("pinterest.bookmark"))?;
        bounds.text("pinterest.bookmark", value, bounds.max_text_bytes)?;
        if value.chars().any(char::is_control) || result.iter().any(|old| old == value) {
            return Err(invalid("pinterest.bookmark"));
        }
        result.push(value.to_owned());
    }
    if result.is_empty() {
        return Ok(None);
    }
    if result
        .iter()
        .any(|value| value == "-end-" || value.starts_with("Y2JOb25lO"))
    {
        if result.len() != 1 {
            return Err(invalid("pinterest.ambiguous_terminal"));
        }
        return Ok(None);
    }
    Ok(Some(result))
}
pub fn discovered_boards(
    bytes: &[u8],
    max: usize,
    max_entries: usize,
    bounds: &Bounds,
) -> Result<(Vec<BoardEvidence>, Option<Vec<String>>, String)> {
    let root = json(bytes, max)?;
    let mut result: Vec<BoardEvidence> = vec![];
    for value in values(&root, max_entries)? {
        let board = public_board(value, bounds)?;
        if let Some(previous) = result.iter().find(|old| old.id == board.id) {
            if previous != &board {
                return Err(invalid("pinterest.retargeted_board"));
            }
        } else {
            result.push(board);
        }
    }
    Ok((
        result,
        bookmarks(&root, bounds)?,
        format!("{:x}", Sha256::digest(bytes)),
    ))
}
pub fn page(bytes: &[u8], max: usize, max_entries: usize, bounds: &Bounds) -> Result<Document> {
    let root = json(bytes, max)?;
    let digest = format!("{:x}", Sha256::digest(bytes));
    let mut entries: Vec<EntryEvidence> = vec![];
    for value in values(&root, max_entries)? {
        let entry = entry(value, &digest, bounds)?;
        if let Some(prior) = entries.iter().find(|old| old.id == entry.id) {
            if prior != &entry {
                return Err(invalid("pinterest.repeated_pin"));
            }
        } else {
            entries.push(entry);
        }
    }
    let next = bookmarks(&root, bounds)?;
    if entries.is_empty() && next.is_some() {
        return Err(invalid("pinterest.empty_continues"));
    }
    Ok(Document {
        entries,
        next,
        digest,
    })
}
pub fn pin(bytes: &[u8], expected: &str, max: usize, bounds: &Bounds) -> Result<Document> {
    let root = json(bytes, max)?;
    let digest = format!("{:x}", Sha256::digest(bytes));
    let entry = entry(&root["resource_response"]["data"], &digest, bounds)?;
    if entry.id != expected {
        return Err(Error::StaleResult);
    }
    Ok(Document {
        entries: vec![entry],
        next: None,
        digest,
    })
}
pub(crate) fn publication(raw: &str) -> Result<hsscrap::PublicationTime> {
    let rfc3339 = chrono::DateTime::parse_from_rfc3339(raw).ok();
    let is_rfc3339 = rfc3339.is_some();
    let date = rfc3339
        .or_else(|| chrono::DateTime::parse_from_rfc2822(raw).ok())
        .ok_or_else(|| invalid("pinterest.publication"))?;
    if date.offset().local_minus_utc() % 60 != 0 {
        return Err(invalid("pinterest.publication_offset"));
    }
    let fraction = raw.get(19..).and_then(|v| v.strip_prefix('.')).map(|v| {
        v.chars()
            .take_while(|v| v.is_ascii_digit())
            .collect::<String>()
    });
    let (precision, local_value) = if let Some(fraction) = fraction {
        if fraction.is_empty() || fraction.len() > 9 {
            return Err(invalid("pinterest.publication_precision"));
        }
        (
            hsscrap::PublicationPrecision::Subsecond,
            format!("{}.{fraction}", date.format("%Y-%m-%dT%H:%M:%S")),
        )
    } else if !is_rfc3339
        && raw
            .split_ascii_whitespace()
            .find(|v| v.contains(':'))
            .is_some_and(|v| v.matches(':').count() == 1)
    {
        (
            hsscrap::PublicationPrecision::Minute,
            date.format("%Y-%m-%dT%H:%M").to_string(),
        )
    } else {
        (
            hsscrap::PublicationPrecision::Second,
            date.format("%Y-%m-%dT%H:%M:%S").to_string(),
        )
    };
    let time = hsscrap::PublicationTime {
        raw_value: raw.into(),
        precision,
        local_value,
        offset_minutes: if raw.ends_with("-00:00") || raw.ends_with("-0000") {
            None
        } else {
            Some((date.offset().local_minus_utc() / 60) as i16)
        },
    };
    time.validate()?;
    Ok(time)
}
fn image(value: &Value, key: MemberKey, bounds: &Bounds) -> Result<MediaEvidence> {
    let url = text(value, "url")?.to_owned();
    public::public_url(&url, bounds)?;
    let dimension = |key| {
        value
            .get(key)
            .filter(|v| !v.is_null())
            .map(|v| {
                v.as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .filter(|n| *n > 0)
                    .ok_or_else(|| invalid("pinterest.dimensions"))
            })
            .transpose()
    };
    Ok(MediaEvidence {
        key,
        kind: hsscrap::MediaKind::Image,
        url,
        width: dimension("width")?,
        height: dimension("height")?,
        duration_ms: None,
    })
}
fn images(value: &Value, key: MemberKey, bounds: &Bounds) -> Result<MediaEvidence> {
    let map = value
        .as_object()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| invalid("pinterest.images"))?;
    if map.len() > bounds.max_collection_items {
        return Err(invalid("pinterest.images"));
    }
    let mut selected: Option<MediaEvidence> = None;
    for (name, value) in map {
        let candidate = image(value, key.clone(), bounds)?;
        if name == "orig" || name == "originals" {
            selected = Some(candidate);
            break;
        }
        if selected
            .as_ref()
            .is_none_or(|old| (candidate.width, candidate.height) > (old.width, old.height))
        {
            selected = Some(candidate);
        }
    }
    selected.ok_or_else(|| invalid("pinterest.images"))
}
fn video(value: &Value, key: MemberKey, bounds: &Bounds) -> Result<MediaEvidence> {
    let formats = value["video_list"]
        .as_object()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| invalid("pinterest.video_list"))?;
    if formats.len() > bounds.max_collection_items {
        return Err(invalid("pinterest.video_list"));
    }
    let mut selected: Option<MediaEvidence> = None;
    for value in formats.values() {
        let mut candidate = image(value, key.clone(), bounds)?;
        candidate.kind = hsscrap::MediaKind::Video;
        candidate.duration_ms = value
            .get("duration_ms")
            .filter(|v| !v.is_null())
            .map(|v| v.as_u64().ok_or_else(|| invalid("pinterest.duration_ms")))
            .transpose()?;
        if selected
            .as_ref()
            .is_none_or(|old| (candidate.width, candidate.height) > (old.width, old.height))
        {
            selected = Some(candidate);
        }
    }
    selected.ok_or_else(|| invalid("pinterest.video_list"))
}
pub(crate) fn entry(value: &Value, digest: &str, bounds: &Bounds) -> Result<EntryEvidence> {
    if value
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|v| v != "pin")
    {
        return Err(invalid("pinterest.item_type"));
    }
    if value.get("is_secret").and_then(Value::as_bool) == Some(true)
        || value
            .get("privacy")
            .and_then(Value::as_str)
            .is_some_and(|v| !v.eq_ignore_ascii_case("public"))
    {
        return Err(Error::Denied);
    }
    let id = id(value.get("id").ok_or_else(|| invalid("pinterest.pin_id"))?)?;
    let mut media = vec![];
    let empty_key = || MemberKey {
        page_id: None,
        media_id: None,
    };
    if let Some(story) = value.get("story_pin_data").filter(|v| !v.is_null()) {
        let pages = story["pages"]
            .as_array()
            .ok_or_else(|| invalid("pinterest.story_pages"))?;
        if pages.len() > bounds.max_collection_items {
            return Err(invalid("pinterest.story_pages"));
        }
        for page in pages {
            let page_id = optional_text(page, "id")?;
            let blocks = page["blocks"]
                .as_array()
                .ok_or_else(|| invalid("pinterest.story_blocks"))?;
            if blocks.len() > bounds.max_collection_items {
                return Err(invalid("pinterest.story_blocks"));
            }
            for block in blocks {
                let key = MemberKey {
                    page_id: page_id.clone(),
                    media_id: optional_text(block, "image_signature")?.or_else(|| {
                        page.get("image_signature")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    }),
                };
                let item = match text(block, "type")? {
                    "story_pin_image_block" => {
                        image(&block["image"]["images"]["originals"], key, bounds)?
                    }
                    "story_pin_video_block" => {
                        let mut key = key;
                        key.media_id = optional_text(&block["video"], "id")?;
                        video(&block["video"], key, bounds)?
                    }
                    "story_pin_music_block" => {
                        let audio = &block["audio"];
                        let url = text(audio, "audio_url")?.to_owned();
                        public::public_url(&url, bounds)?;
                        MediaEvidence {
                            key: MemberKey {
                                page_id: page_id.clone(),
                                media_id: optional_text(audio, "id")?,
                            },
                            kind: hsscrap::MediaKind::Audio,
                            url,
                            width: None,
                            height: None,
                            duration_ms: None,
                        }
                    }
                    "story_pin_paragraph_block"
                    | "story_pin_product_sticker_block"
                    | "story_pin_static_sticker_block" => continue,
                    _ => return Err(invalid("pinterest.unsupported_story_block")),
                };
                media.push(item);
            }
        }
    } else if let Some(carousel) = value.get("carousel_data").filter(|v| !v.is_null()) {
        let slots = carousel["carousel_slots"]
            .as_array()
            .ok_or_else(|| invalid("pinterest.carousel_slots"))?;
        if slots.len() > bounds.max_collection_items {
            return Err(invalid("pinterest.carousel_slots"));
        }
        for slot in slots {
            media.push(images(
                &slot["images"],
                MemberKey {
                    page_id: None,
                    media_id: optional_text(slot, "id")?,
                },
                bounds,
            )?);
        }
    } else if let Some(videos) = value.get("videos").filter(|v| !v.is_null()) {
        media.push(video(videos, empty_key(), bounds)?);
    } else {
        media.push(images(&value["images"], empty_key(), bounds)?);
    }
    let entry = EntryEvidence {
        id,
        title: optional_text(value, "title")?,
        description: optional_text(value, "description")?,
        author: optional_text(&value["pinner"], "username")?,
        published_at: optional_text(value, "created_at")?
            .map(|raw| publication(&raw))
            .transpose()?,
        outbound_url: optional_text(value, "link")?.filter(|v| !v.is_empty()),
        media,
        document_digest: digest.into(),
    };
    entry.validate(bounds)?;
    Ok(entry)
}
