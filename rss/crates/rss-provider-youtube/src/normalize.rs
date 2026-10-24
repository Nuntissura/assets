use std::{collections::{BTreeMap,BTreeSet},sync::{Arc,Mutex}};
use chrono::{DateTime,Utc};
use rss_contracts::{Bounds,Error,Result};
use serde_json::Value;
use sha2::{Digest,Sha256};
use crate::{EntryEvidence,Preview,invalid,public};

pub struct Channel { pub id:String,pub uploads:String }
pub struct Document { pub entries:Vec<EntryEvidence>,pub next:Option<String>,pub digest:String,pub verified_empty:bool }
pub fn json(bytes:&[u8],max_bytes:usize)->Result<Value> {
    // Admit through the existing duplicate/depth boundary, then parse original bytes without JCS number conversion.
    handshake_canonical_json::jcs_from_slice(bytes,max_bytes).map_err(|_|invalid("youtube.json"))?;
    serde_json::from_slice(bytes).map_err(|_|invalid("youtube.json"))
}
fn text<'a>(value:&'a Value,key:&str)->Result<&'a str> {value.get(key).and_then(Value::as_str).ok_or_else(||invalid(key))}
fn items<'a>(root:&'a Value,kind:&str,max:usize)->Result<&'a Vec<Value>> {
    if text(root,"kind")?!=kind {return Err(invalid("youtube.resource_kind"));}
    let items=root.get("items").and_then(Value::as_array).ok_or_else(||invalid("youtube.items"))?;
    if items.len()>max {return Err(Error::LimitExceeded {field:"youtube.items".into()});} Ok(items)
}
pub fn channel(bytes:&[u8],expected:Option<&str>,max_bytes:usize)->Result<Channel> {
    let root=json(bytes,max_bytes)?; let items=items(&root,"youtube#channelListResponse",1)?;
    if items.len()!=1 {return Err(Error::NotFound);}
    let id=text(&items[0],"id")?;
    let uploads=text(&items[0]["contentDetails"]["relatedPlaylists"],"uploads")?;
    if !public::channel_id(id)||!public::playlist_id(uploads)||expected.is_some_and(|expected|expected!=id) {return Err(invalid("youtube.channel_identity"));}
    Ok(Channel {id:id.into(),uploads:uploads.into()})
}
pub fn publication(raw:&str)->Result<hsscrap::PublicationTime> {
    let date=DateTime::parse_from_rfc3339(raw).map_err(|_|invalid("youtube.publication"))?;
    if date.offset().local_minus_utc()%60!=0 {return Err(invalid("youtube.publication_offset"));}
    let fraction=raw.get(19..).and_then(|s|s.strip_prefix('.')).map(|s|s.chars().take_while(|c|c.is_ascii_digit()).collect::<String>());
    let (precision,local_value)=if let Some(fraction)=fraction {
        if fraction.is_empty()||fraction.len()>9 {return Err(invalid("youtube.publication_precision"));}
        (hsscrap::PublicationPrecision::Subsecond,format!("{}.{fraction}",date.format("%Y-%m-%dT%H:%M:%S")))
    } else {(hsscrap::PublicationPrecision::Second,date.format("%Y-%m-%dT%H:%M:%S").to_string())};
    let time=hsscrap::PublicationTime {raw_value:raw.into(),precision,local_value,
        offset_minutes:if raw.ends_with("-00:00") {None} else {Some((date.offset().local_minus_utc()/60) as i16)}};
    time.validate()?;Ok(time)
}
fn previews(value:&Value,bounds:&Bounds)->Result<Vec<Preview>> {
    let Some(map)=value.as_object() else {if value.is_null() {return Ok(vec![]);} return Err(invalid("youtube.thumbnails"));};
    if map.len()>bounds.max_collection_items {return Err(invalid("youtube.thumbnails"));}
    let mut result=vec![];
    for value in map.values() {
        let url=text(value,"url")?.to_owned();public::preview_url(&url,bounds)?;
        let dimension=|key| value.get(key).map(|v|v.as_u64().and_then(|n|u32::try_from(n).ok()).filter(|n|*n>0).ok_or_else(||invalid("youtube.thumbnail_dimensions"))).transpose();
        let preview=Preview {url,width:dimension("width")?,height:dimension("height")?};
        if !result.contains(&preview) {result.push(preview);}
    }
    Ok(result)
}
pub fn playlist(bytes:&[u8],channel:&Channel,max_bytes:usize,max_entries:usize,bounds:&Bounds)->Result<Document> {
    let root=json(bytes,max_bytes)?;let values=items(&root,"youtube#playlistItemListResponse",max_entries)?;
    let next=root.get("nextPageToken").map(|value|value.as_str().filter(|s|!s.is_empty()).ok_or_else(||invalid("youtube.next_token"))).transpose()?.map(str::to_owned);
    if let Some(next)=&next {bounds.text("youtube.next_token",next,bounds.max_text_bytes)?;if next.chars().any(char::is_control) {return Err(invalid("youtube.next_token"));}}
    if values.is_empty()&&next.is_some() {return Err(invalid("youtube.empty_continues"));}
    let digest=format!("{:x}",Sha256::digest(bytes));let mut entries:Vec<EntryEvidence>=vec![];let mut playlist_items=BTreeSet::new();
    for value in values {
        if text(value,"kind")?!="youtube#playlistItem"||!playlist_items.insert(text(value,"id")?) {return Err(invalid("youtube.playlist_item"));}
        let snippet=&value["snippet"];let content=&value["contentDetails"];
        if text(snippet,"playlistId")?!=channel.uploads||text(snippet,"channelId")?!=channel.id
            ||text(&snippet["resourceId"],"kind")?!="youtube#video"||text(&value["status"],"privacyStatus")?!="public" {return Err(Error::Denied);}
        let id=text(content,"videoId")?;
        if text(&snippet["resourceId"],"videoId")?!=id||snippet.get("videoOwnerChannelId").is_some_and(|owner|owner.as_str()!=Some(channel.id.as_str())) {
            return Err(invalid("youtube.video_identity"));
        }
        let entry=EntryEvidence {video_id:id.into(),channel_id:channel.id.clone(),title:Some(text(snippet,"title")?.into()),description:Some(text(snippet,"description")?.into()),
            author:snippet.get("videoOwnerChannelTitle").or_else(||snippet.get("channelTitle")).map(|v|v.as_str().map(str::to_owned).ok_or_else(||invalid("youtube.author"))).transpose()?,
            published_at:content.get("videoPublishedAt").map(|v|publication(v.as_str().ok_or_else(||invalid("youtube.video_publication"))?)).transpose()?,
            previews:previews(&snippet["thumbnails"],bounds)?,document_digest:digest.clone()};
        entry.validate(bounds)?;
        if let Some(prior)=entries.iter().find(|old|old.video_id==entry.video_id) {if prior!=&entry {return Err(invalid("youtube.repeated_video"));}}
        else {entries.push(entry);}
    }
    let verified_empty=values.is_empty()&&next.is_none()&&root["pageInfo"]["totalResults"].as_u64()==Some(0);
    Ok(Document {entries,next,digest,verified_empty})
}
pub fn video(bytes:&[u8],id:&str,max_bytes:usize,bounds:&Bounds)->Result<Document> {
    let root=json(bytes,max_bytes)?;let values=items(&root,"youtube#videoListResponse",1)?;
    if values.len()!=1 {return Err(Error::NotFound);}
    let item=&values[0];if text(item,"kind")?!="youtube#video"||text(item,"id")?!=id {return Err(invalid("youtube.video_identity"));}
    if text(&item["status"],"privacyStatus")?!="public" {return Err(Error::Denied);}
    let snippet=&item["snippet"];let digest=format!("{:x}",Sha256::digest(bytes));
    let entry=EntryEvidence {video_id:id.into(),channel_id:text(snippet,"channelId")?.into(),title:Some(text(snippet,"title")?.into()),description:Some(text(snippet,"description")?.into()),
        author:Some(text(snippet,"channelTitle")?.into()),published_at:Some(publication(text(snippet,"publishedAt")?)?),previews:previews(&snippet["thumbnails"],bounds)?,document_digest:digest.clone()};
    entry.validate(bounds)?;Ok(Document {entries:vec![entry],next:None,digest,verified_empty:false})
}
pub fn atom(bytes:&[u8],channel:&str,max_bytes:usize,max_entries:usize,bounds:&Bounds)->Result<Document> {
    if bytes.len()>max_bytes {return Err(Error::LimitExceeded {field:"youtube.atom_bytes".into()});}
    let dates:Arc<Mutex<BTreeMap<String,Vec<hsscrap::PublicationTime>>>>=Arc::default();let captured=dates.clone();
    let parser=feed_rs::parser::Builder::new().id_generator(|_,_,_|String::new()).timestamp_parser(move |raw| {
        let time=publication(raw).ok()?;let utc=DateTime::parse_from_rfc3339(raw).ok()?.with_timezone(&Utc);
        let mut map=captured.lock().ok()?;let values=map.entry(utc.to_rfc3339()).or_default();if !values.contains(&time) {values.push(time);}Some(utc)
    }).build();
    let feed=parser.parse(bytes).map_err(|_|invalid("youtube.atom_parse"))?;
    if feed.feed_type!=feed_rs::model::FeedType::Atom||!public::channel_id(channel) {return Err(invalid("youtube.atom_channel"));}
    let full_channel=feed.id==format!("yt:channel:{channel}");
    let channel_tail=feed.id==format!("yt:channel:{}",&channel[2..])&&feed.links.iter().any(|link|
        link.rel.as_deref()==Some("alternate")&&matches!(public::resolve_url(&link.href,bounds),Ok(public::SourceTarget::Channel(id)) if id==channel));
    if !full_channel&&!channel_tail {return Err(invalid("youtube.atom_channel"));}
    if feed.entries.len()>max_entries {return Err(Error::LimitExceeded {field:"youtube.atom_entries".into()});}
    if feed.links.iter().any(|link|link.rel.as_deref()==Some("next")) {return Err(invalid("youtube.atom_unexpected_pagination"));}
    let digest=format!("{:x}",Sha256::digest(bytes));let mut entries:Vec<EntryEvidence>=vec![];
    for entry in feed.entries {
        let id=entry.id.strip_prefix("yt:video:").filter(|id|public::video_id(id)).ok_or_else(||invalid("youtube.atom_video"))?;
        if !entry.links.iter().any(|link|matches!(link.rel.as_deref(),None|Some("alternate"))
            &&matches!(public::resolve_url(&link.href,bounds),Ok(public::SourceTarget::Video(video)) if video==id)) {return Err(invalid("youtube.atom_permalink"));}
        let mut images=vec![];
        for media in &entry.media {for image in &media.thumbnails {
            let preview=Preview {url:image.image.uri.clone(),width:image.image.width,height:image.image.height};
            if !images.contains(&preview) {images.push(preview);}
        }}
        let published_at=entry.published.as_ref().and_then(|date|dates.lock().ok()?.get(&date.to_rfc3339()).filter(|values|values.len()==1).and_then(|values|values.first()).cloned());
        let value=EntryEvidence {video_id:id.into(),channel_id:channel.into(),title:entry.title.map(|v|v.content),description:entry.media.iter().find_map(|v|v.description.as_ref().map(|v|v.content.clone())),
            author:entry.authors.first().and_then(|author|author.name.clone()),published_at,previews:images,document_digest:digest.clone()};
        value.validate(bounds)?;
        if let Some(prior)=entries.iter().find(|old|old.video_id==value.video_id) {if prior!=&value {return Err(invalid("youtube.repeated_video"));}}
        else {entries.push(value);}
    }
    Ok(Document {entries,next:None,digest,verified_empty:false})
}
