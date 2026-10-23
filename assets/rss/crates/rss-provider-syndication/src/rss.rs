use std::{collections::{BTreeMap, BTreeSet}, sync::{Arc, Mutex}};
use chrono::{DateTime, Utc};
use feed_rs::model::{Entry, FeedType};
use hsscrap::{MediaKind, PublicationPrecision, PublicationTime};
use rss_contracts::{Bounds, Error, Result};
use scraper::Html;
use sha2::{Digest, Sha256};
use crate::{blogs::public_url, invalid, EntryEvidence, MediaEvidence};

pub struct FeedDocument {
    pub entries: Vec<EntryEvidence>,
    pub next_document: Option<String>,
    pub digest: String,
}

fn plain(value: &str, bounds: &Bounds) -> Result<String> {
    let html = Html::parse_fragment(value);
    let result = html.root_element().text().collect::<String>();
    bounds.text("metadata.text", &result, bounds.max_text_bytes)?;
    Ok(result)
}
fn media_kind(mime: Option<&str>) -> MediaKind {
    match mime.unwrap_or("").split('/').next().unwrap_or("") {
        "image" => MediaKind::Image, "video" => MediaKind::Video, "audio" => MediaKind::Audio,
        "application" | "text" => MediaKind::Document, _ => MediaKind::Unsupported,
    }
}
fn append_media(result: &mut Vec<MediaEvidence>, value: MediaEvidence, bounds: &Bounds) -> Result<()> {
    public_url(&value.url, bounds)?;
    if value.width == Some(0) || value.height == Some(0) { return Err(invalid("media.dimensions")); }
    if let Some(previous) = result.iter_mut().find(|item| item.url == value.url && item.thumbnail == value.thumbnail) {
        fn merge<T: PartialEq>(old: &mut Option<T>, new: Option<T>) -> Result<()> {
            match (old.as_ref(), new) { (Some(old), Some(new)) if old != &new => Err(Error::Conflict),
                (None, new) => { *old = new; Ok(()) }, _ => Ok(()) }
        }
        if previous.kind == MediaKind::Unsupported { previous.kind = value.kind; }
        else if value.kind != MediaKind::Unsupported && previous.kind != value.kind { return Err(Error::Conflict); }
        merge(&mut previous.mime, value.mime)?; merge(&mut previous.caption, value.caption)?;
        merge(&mut previous.width, value.width)?; merge(&mut previous.height, value.height)?;
        merge(&mut previous.duration_ms, value.duration_ms)?; merge(&mut previous.size_bytes, value.size_bytes)?;
        return Ok(());
    }
    if result.len() == bounds.max_collection_items { return Err(Error::LimitExceeded { field: "media".into() }); }
    result.push(value); Ok(())
}
fn media(entry: &Entry, bounds: &Bounds) -> Result<Vec<MediaEvidence>> {
    let mut result = vec![];
    let enclosure = entry.content.as_ref().and_then(|content| content.src.as_ref());
    for link in entry.links.iter().filter(|link| link.rel.as_deref() == Some("enclosure")).chain(enclosure) {
        let url = public_url(&link.href, bounds)?.to_string();
        append_media(&mut result, MediaEvidence { url, kind: media_kind(link.media_type.as_deref()), mime: link.media_type.clone(), caption: None,
            width: None, height: None, duration_ms: None, size_bytes: link.length, thumbnail: false }, bounds)?;
    }
    for object in &entry.media {
        for content in &object.content {
            if let Some(url) = &content.url {
                let mime = content.content_type.as_ref().map(ToString::to_string);
                let duration_ms = content.duration.or(object.duration).map(|duration| u64::try_from(duration.as_millis()).map_err(|_| invalid("media.duration"))).transpose()?;
                let caption = object.description.as_ref().or(object.title.as_ref()).map(|value| plain(&value.content, bounds)).transpose()?;
                append_media(&mut result, MediaEvidence { url: url.to_string(), kind: media_kind(mime.as_deref()), mime,
                    caption, width: content.width, height: content.height, duration_ms, size_bytes: content.size, thumbnail: false }, bounds)?;
            }
        }
        for thumbnail in &object.thumbnails {
            append_media(&mut result, MediaEvidence { url: thumbnail.image.uri.clone(), kind: MediaKind::Image, mime: None,
                caption: thumbnail.image.title.clone(), width: thumbnail.image.width, height: thumbnail.image.height,
                duration_ms: None, size_bytes: None, thumbnail: true }, bounds)?;
        }
    }
    Ok(result)
}

/// Parse bounded encoded bytes, keeping only explicit provider IDs and unambiguous source date literals.
pub fn parse(document_url: &str, bytes: &[u8], max_bytes: usize, max_entries: usize, bounds: &Bounds) -> Result<FeedDocument> {
    bounds.validate()?;
    let base = public_url(document_url, bounds)?;
    if max_bytes == 0 || bytes.len() > max_bytes { return Err(Error::LimitExceeded { field: "feed.body".into() }); }
    if max_entries == 0 || max_entries > 10_000 { return Err(invalid("feed.max_entries")); }
    let timestamps: Arc<Mutex<BTreeMap<String, Vec<PublicationTime>>>> = Arc::default();
    let captured = timestamps.clone();
    let parser = feed_rs::parser::Builder::new().base_uri(Some(base.as_str()))
        .id_generator(|_, _, _| String::new())
        .timestamp_parser(move |raw| {
            let rfc3339 = DateTime::parse_from_rfc3339(raw).ok();
            let is_rfc3339 = rfc3339.is_some();
            let date = rfc3339.or_else(|| DateTime::parse_from_rfc2822(raw).ok())?;
            if date.offset().local_minus_utc() % 60 != 0 { return None; }
            let fraction = raw.get(19..).and_then(|value| value.strip_prefix('.'))
                .map(|value| value.chars().take_while(|value| value.is_ascii_digit()).collect::<String>());
            let (precision, local_value) = if let Some(fraction) = fraction {
                if fraction.is_empty() || fraction.len() > 9 { return None; }
                (PublicationPrecision::Subsecond, format!("{}.{}", date.format("%Y-%m-%dT%H:%M:%S"), fraction))
            } else if !is_rfc3339 && raw.split_ascii_whitespace().find(|value| value.contains(':'))?.matches(':').count() == 1 {
                (PublicationPrecision::Minute, date.format("%Y-%m-%dT%H:%M").to_string())
            } else { (PublicationPrecision::Second, date.format("%Y-%m-%dT%H:%M:%S").to_string()) };
            // Minus-zero denotes an unknown local offset, not known UTC (RFC3339/RFC5322).
            let unknown_offset = is_rfc3339 && raw.ends_with("-00:00")
                || !is_rfc3339 && raw.split_ascii_whitespace().any(|value| value == "-0000");
            if date.timestamp_subsec_nanos() >= 1_000_000_000 && local_value.get(17..19) != Some("60") { return None; }
            let publication = PublicationTime { raw_value: raw.into(), local_value, precision,
                offset_minutes: if unknown_offset { None } else { Some((date.offset().local_minus_utc() / 60).try_into().ok()?) } };
            publication.validate().ok()?;
            let utc = date.with_timezone(&Utc);
            let mut values = captured.lock().ok()?;
            let literals = values.entry(utc.to_rfc3339()).or_default();
            if !literals.contains(&publication) { literals.push(publication); }
            Some(utc)
        }).build();
    let feed = parser.parse(bytes).map_err(|_| invalid("feed.parse"))?;
    if !matches!(feed.feed_type, FeedType::Atom | FeedType::RSS0 | FeedType::RSS1 | FeedType::RSS2) { return Err(invalid("feed.format")); }
    if feed.entries.len() > max_entries { return Err(Error::LimitExceeded { field: "feed.entries".into() }); }
    let digest = format!("{:x}", Sha256::digest(bytes));
    let mut entries = vec![];
    let mut keys: BTreeMap<String, EntryEvidence> = BTreeMap::new();
    for entry in feed.entries {
        let link = entry.links.iter().find(|link| matches!(link.rel.as_deref(), None | Some("alternate"))
            && link.media_type.as_deref().is_none_or(|mime| mime == "text/html" || mime == "application/xhtml+xml"))
            .map(|link| link.href.as_str()).or_else(|| (!entry.id.is_empty()).then_some(entry.id.as_str()))
            .ok_or_else(|| invalid("entry.link"))?;
        let url = public_url(base.join(link).map_err(|_| invalid("entry.link"))?.as_str(), bounds)?.to_string();
        let canonical = hsscrap::identity::normalize_url(&url, bounds)?.identity_url;
        let reliable_provider_id = (!entry.id.is_empty()).then(|| entry.id.clone());
        let key = reliable_provider_id.clone().unwrap_or(canonical);
        let title = entry.title.as_ref().map(|value| plain(&value.content, bounds)).transpose()?;
        let description = entry.summary.as_ref().map(|value| value.content.as_str())
            .or_else(|| entry.content.as_ref().and_then(|value| value.body.as_deref())).map(|value| plain(value, bounds)).transpose()?;
        if entry.authors.len() > bounds.max_collection_items { return Err(Error::LimitExceeded { field: "entry.authors".into() }); }
        let names: Vec<_> = entry.authors.iter().filter_map(|value| value.name.as_deref()).filter(|value| !value.is_empty()).collect();
        let author = (!names.is_empty()).then(|| names.join(", "));
        let published_at = entry.published.as_ref().and_then(|date| timestamps.lock().ok()?.get(&date.to_rfc3339())
            .filter(|values| values.len() == 1).and_then(|values| values.first()).cloned());
        let labels: BTreeSet<_> = entry.categories.iter().map(|value| value.term.clone()).collect();
        let host_site = public_url(&url, bounds)?.host_str().ok_or_else(|| invalid("entry.host"))?.to_owned();
        let evidence = EntryEvidence { key: key.clone(), url, reliable_provider_id, host_site, title, description, author,
            published_at, labels: labels.into_iter().collect(), media: media(&entry, bounds)?, media_coverage: hsscrap::Coverage::Partial, document_digest: digest.clone() };
        evidence.validate(bounds)?;
        if let Some(previous) = keys.get(&key) {
            if previous != &evidence { return Err(Error::Conflict); }
        } else { keys.insert(key, evidence.clone()); entries.push(evidence); }
    }
    let mut next = BTreeSet::new();
    for link in feed.links.iter().filter(|link| link.rel.as_deref() == Some("next")) {
        next.insert(public_url(base.join(&link.href).map_err(|_| invalid("feed.next"))?.as_str(), bounds)?.to_string());
    }
    if next.len() > 1 { return Err(invalid("feed.ambiguous_next")); }
    Ok(FeedDocument { entries, next_document: next.into_iter().next(), digest })
}
