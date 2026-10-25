use rss_contracts::{Bounds,Error,Result};
use serde_json::Value;
use sha2::{Digest,Sha256};
use scraper::{Html,Selector};
use crate::{EntryEvidence,MediaEvidence,invalid,public};
pub const MIN_CURSOR:u64=1_472_706_000_000;
const WEEK:u64=7*86_400_000;
pub struct Account {pub id:String,pub sec_uid:String,pub name:String,pub video_count:Option<u64>}
pub struct Document {pub entries:Vec<EntryEvidence>,pub next:Option<u64>,pub digest:String,pub terminal:bool}
pub fn json(bytes:&[u8],max:usize)->Result<Value> {
    handshake_canonical_json::jcs_from_slice(bytes,max).map_err(|_|invalid("tiktok.json"))?;
    serde_json::from_slice(bytes).map_err(|_|invalid("tiktok.json"))
}
fn text<'a>(value:&'a Value,key:&str)->Result<&'a str> {value.get(key).and_then(Value::as_str).ok_or_else(||invalid(key))}
pub(crate) fn unsigned(value:&Value)->Result<u64> {
    if let Some(n)=value.as_u64() {Ok(n)} else {let raw=value.as_str().ok_or_else(||invalid("tiktok.integer"))?;
        if raw.is_empty()||!raw.bytes().all(|v|v.is_ascii_digit())||(raw.len()>1&&raw.starts_with('0')) {return Err(invalid("tiktok.integer"));}
        raw.parse().map_err(|_|invalid("tiktok.integer"))}
}
fn numeric(value:&Value)->Result<String> {
    let value=if let Some(value)=value.as_str() {value.to_owned()} else {unsigned(value)?.to_string()};
    if !public::numeric_id(&value) {return Err(invalid("tiktok.numeric_id"));}Ok(value)
}
pub(crate) fn status(value:&Value)->Result<()> {
    match value.get("statusCode").and_then(Value::as_u64) {
        Some(0)=>Ok(()),Some(10216|10222|10204)=>Err(Error::Denied),_=>Err(invalid("tiktok.status")),
    }
}
fn rehydration(bytes:&[u8],max:usize)->Result<Value> {
    if bytes.len()>max {return Err(Error::LimitExceeded {field:"tiktok.html".into()});}
    let html=std::str::from_utf8(bytes).map_err(|_|invalid("tiktok.html"))?;
    let document=Html::parse_document(html);let selector=Selector::parse("script#__UNIVERSAL_DATA_FOR_REHYDRATION__").map_err(|_|invalid("tiktok.selector"))?;
    let mut scripts=document.select(&selector);let script=scripts.next().ok_or(Error::Challenged)?;
    if scripts.next().is_some() {return Err(invalid("tiktok.ambiguous_rehydration"));}
    let raw=script.text().collect::<String>();let root=json(raw.as_bytes(),max)?;
    root.get("__DEFAULT_SCOPE__").filter(|value|value.is_object()).cloned().ok_or_else(||invalid("tiktok.rehydration_scope"))
}
pub fn account(bytes:&[u8],name:&str,max:usize,bounds:&Bounds)->Result<Account> {
    let root=rehydration(bytes,max)?;let detail=&root["webapp.user-detail"];status(detail)?;
    let user=&detail["userInfo"]["user"];
    if user.get("privateAccount").and_then(Value::as_bool)!=Some(false) {return Err(Error::Denied);}
    account_identity(detail,name,bounds)
}
pub(crate) fn connected_account(bytes:&[u8],name:&str,max:usize,bounds:&Bounds,mode:rss_contracts::AccessMode,viewer_id:&str,viewer_sec:&str)->Result<Account> {
    let root=rehydration(bytes,max)?;let detail=&root["webapp.user-detail"];status(detail)?;
    let user=&detail["userInfo"]["user"];
    let private=user.get("privateAccount").and_then(Value::as_bool).ok_or(Error::Denied)?;
    let account=account_identity(detail,name,bounds)?;
    let own=account.id==viewer_id&&account.sec_uid==viewer_sec;
    match mode {
        rss_contracts::AccessMode::OwnUploads if own=>{},
        rss_contracts::AccessMode::PrivateSource if private&&(own||user.get("relation").and_then(Value::as_u64).is_some_and(|v|matches!(v,1|3)))=>{},
        _=>return Err(Error::Denied),
    }
    Ok(account)
}
fn account_identity(detail:&Value,name:&str,bounds:&Bounds)->Result<Account> {
    let user=&detail["userInfo"]["user"];
    let actual=text(user,"uniqueId")?;
    if !actual.eq_ignore_ascii_case(name) {return Err(Error::StaleResult);}
    let id=numeric(user.get("id").ok_or_else(||invalid("tiktok.account_id"))?)?;let sec_uid=text(user,"secUid")?.to_owned();
    if !public::sec_uid(&sec_uid) {return Err(invalid("tiktok.sec_uid"));}bounds.text("tiktok.sec_uid",&sec_uid,bounds.max_text_bytes)?;
    let video_count=detail["userInfo"].get("stats").or_else(||detail["userInfo"].get("statsV2")).and_then(|stats|stats.get("videoCount")).map(unsigned).transpose()?;
    Ok(Account {id,sec_uid,name:actual.into(),video_count})
}
pub fn window(bytes:&[u8],account:&Account,cursor:u64,max:usize,max_entries:usize,bounds:&Bounds)->Result<Document> {
    let root=json(bytes,max)?;status(&root)?;
    let values=root.get("itemList").and_then(Value::as_array).ok_or_else(||invalid("tiktok.item_list"))?;
    if values.len()>max_entries {return Err(Error::LimitExceeded {field:"tiktok.items".into()});}
    let more=root.get("hasMorePrevious").and_then(Value::as_bool).ok_or_else(||invalid("tiktok.more_previous"))?;
    let digest=format!("{:x}",Sha256::digest(bytes));let mut entries:Vec<EntryEvidence>=vec![];
    for value in values {
        let entry=entry(value,&digest,bounds)?;
        if entry.author_id!=account.id||entry.author_sec_uid!=account.sec_uid||!entry.author.eq_ignore_ascii_case(&account.name) {return Err(invalid("tiktok.source_author"));}
        if let Some(prior)=entries.iter().find(|old|old.id==entry.id) {if prior!=&entry {return Err(invalid("tiktok.repeated_item"));}}
        else {entries.push(entry);}
    }
    let next=if more {
        let last=values.last().and_then(|v|v.get("createTime")).map(unsigned).transpose()?.and_then(|n|n.checked_mul(1000));
        let next=match last {Some(n) if n!=0&&n!=cursor=>n,_=>cursor.checked_sub(WEEK).ok_or_else(||invalid("tiktok.cursor_underflow"))?};
        if next>=cursor {return Err(invalid("tiktok.cursor_direction"));}
        (next>=MIN_CURSOR).then_some(next)
    } else {None};
    Ok(Document {entries,next,digest,terminal:!more})
}
pub fn item_document(bytes:&[u8],id:&str,name:&str,max:usize,bounds:&Bounds)->Result<Document> {
    let root=rehydration(bytes,max)?;let detail=&root["webapp.video-detail"];status(detail)?;
    let digest=format!("{:x}",Sha256::digest(bytes));let entry=entry(&detail["itemInfo"]["itemStruct"],&digest,bounds)?;
    if entry.id!=id||!entry.author.eq_ignore_ascii_case(name) {return Err(Error::StaleResult);}
    Ok(Document {entries:vec![entry],next:None,digest,terminal:true})
}
fn dimension(value:&Value,key:&str)->Result<Option<u32>> {value.get(key).filter(|v|!v.is_null()).map(|v|unsigned(v).ok().and_then(|n|u32::try_from(n).ok()).filter(|n|*n>0).ok_or_else(||invalid("tiktok.dimension"))).transpose()}
fn media_url(value:&str,bounds:&Bounds)->Result<(String,Option<u64>)> {
    public::media_url(value,bounds)?;let url=url::Url::parse(value).map_err(|_|invalid("tiktok.media_url"))?;
    let mut expiry=None;
    for (key,value) in url.query_pairs().filter(|(key,_)|matches!(key.as_ref(),"x-expires"|"expire")) {
        let current=value.parse::<u64>().map_err(|_|invalid("tiktok.media_expiry"))?;
        if expiry.is_some_and(|previous|previous!=current) {return Err(invalid("tiktok.media_expiry"));}expiry=Some(current);let _=key;
    }
    Ok((value.into(),expiry))
}
pub(crate) fn entry(value:&Value,digest:&str,bounds:&Bounds)->Result<EntryEvidence> {
    let id=numeric(value.get("id").ok_or_else(||invalid("tiktok.item_id"))?)?;let author=&value["author"];
    let author_id=numeric(author.get("id").ok_or_else(||invalid("tiktok.author_id"))?)?;
    let author_sec_uid=text(author,"secUid")?.to_owned();let author_name=text(author,"uniqueId")?.to_owned();
    let description=text(value,"desc")?.to_owned();
    let create_time_seconds=value.get("createTime").filter(|v|!v.is_null()).map(unsigned).transpose()?;
    let published_at=create_time_seconds.map(|seconds|->Result<_> {
        let date=chrono::DateTime::from_timestamp(i64::try_from(seconds).map_err(|_|invalid("tiktok.create_time"))?,0).ok_or_else(||invalid("tiktok.create_time"))?;
        let date=hsscrap::PublicationTime {raw_value:seconds.to_string(),precision:hsscrap::PublicationPrecision::Second,local_value:date.format("%Y-%m-%dT%H:%M:%S").to_string(),offset_minutes:Some(0)};
        date.validate()?;Ok(date)
    }).transpose()?;
    let mut media=vec![];let photo=value.get("imagePost").is_some_and(|v|!v.is_null());
    if photo {
        let images=value["imagePost"]["images"].as_array().filter(|v|!v.is_empty()).ok_or_else(||invalid("tiktok.images"))?;
        if images.len()>bounds.max_collection_items {return Err(invalid("tiktok.images"));}
        for image in images {
            let urls=image["imageURL"]["urlList"].as_array().filter(|v|!v.is_empty()).ok_or_else(||invalid("tiktok.image_urls"))?;
            if urls.len()>bounds.max_collection_items {return Err(invalid("tiktok.image_urls"));}
            for url in urls {media_url(url.as_str().ok_or_else(||invalid("tiktok.image_url"))?,bounds)?;}
            let (url,expires_at_unix)=media_url(urls[0].as_str().ok_or_else(||invalid("tiktok.image_url"))?,bounds)?;
            media.push(MediaEvidence {provider_member_id:image.get("id").map(|v|v.as_str().map(str::to_owned).ok_or_else(||invalid("tiktok.image_id"))).transpose()?,kind:hsscrap::MediaKind::Image,
                url,preview_url:None,width:dimension(image,"imageWidth")?,height:dimension(image,"imageHeight")?,duration_ms:None,expires_at_unix});
        }
    } else {
        let video=&value["video"];let (url,expires_at_unix)=media_url(text(video,"playAddr")?,bounds)?;
        let preview_url=video.get("cover").filter(|v|!v.is_null()).map(|v|media_url(v.as_str().ok_or_else(||invalid("tiktok.cover"))?,bounds).map(|v|v.0)).transpose()?;
        let duration_ms=video.get("duration").filter(|v|!v.is_null()).map(|v|unsigned(v)?.checked_mul(1000).ok_or_else(||invalid("tiktok.duration"))).transpose()?;
        media.push(MediaEvidence {provider_member_id:video.get("id").filter(|v|!v.is_null()).map(|v|v.as_str().map(str::to_owned).ok_or_else(||invalid("tiktok.video_id"))).transpose()?,
            kind:hsscrap::MediaKind::Video,url,preview_url,width:dimension(video,"width")?,height:dimension(video,"height")?,duration_ms,expires_at_unix});
    }
    let entry=EntryEvidence {id,author_id,author_sec_uid,author:author_name,description,published_at,create_time_seconds,media,photo,document_digest:digest.into()};
    entry.validate(bounds)?;Ok(entry)
}
