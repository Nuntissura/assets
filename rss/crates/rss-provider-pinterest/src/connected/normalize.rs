use super::*;
use crate::{EntryEvidence,MediaEvidence,MemberKey};
use serde_json::Value;
pub(super) struct Board {pub count:Option<u64>,pub privacy:String}
pub(super) struct Document {pub entries:Vec<EntryEvidence>,pub next:Option<String>,pub digest:String}
fn json(bytes:&[u8],max:usize)->rss_contracts::Result<Value>{handshake_canonical_json::jcs_from_slice(bytes,max).map_err(|_|invalid("pinterest.json"))?;let v:Value=serde_json::from_slice(bytes).map_err(|_|invalid("pinterest.json"))?;if v.get("code").is_some()||v.get("error").is_some(){return Err(Error::Denied);}Ok(v)}
fn text<'a>(v:&'a Value,key:&str)->rss_contracts::Result<&'a str>{v.get(key).and_then(Value::as_str).ok_or_else(||invalid("pinterest.document"))}
fn optional(v:&Value,key:&str)->rss_contracts::Result<Option<String>>{v.get(key).filter(|v|!v.is_null()).map(|v|v.as_str().map(str::to_owned).ok_or_else(||invalid("pinterest.text"))).transpose()}
pub(super) fn account(bytes:&[u8],expected:&str,max:usize,b:&Bounds)->rss_contracts::Result<()> {
    let v=json(bytes,max)?;if text(&v,"id")?!=expected{return Err(Error::Denied);}let username=text(&v,"username")?;
    b.text("pinterest.username",username,b.max_text_bytes)?;if username.is_empty(){return Err(invalid("pinterest.username"));}Ok(())
}
pub(super) fn board(bytes:&[u8],a:&AuthorizedContext,mode:AccessMode,max:usize,b:&Bounds)->rss_contracts::Result<Board> {
    let v=json(bytes,max)?;if text(&v,"id")?!=a.selected_board.board_id{return Err(Error::Denied);}
    let public::SourceTarget::Board {owner,..}=public::resolve_url(&a.selected_board.source_url,b)? else{return Err(Error::Denied);};
    if !text(&v["owner"],"username")?.eq_ignore_ascii_case(&owner){return Err(Error::Denied);}
    let privacy=text(&v,"privacy")?;
    match (mode,privacy) {(AccessMode::PublicBoard,"PUBLIC")=>{},(AccessMode::PrivateSource,"SECRET")=>{
        if !a.granted_scopes.contains("boards:read_secret")||!a.granted_scopes.contains("pins:read_secret"){return Err(Error::Denied);}
    },(AccessMode::PrivateSource,"PROTECTED")=>{},_=>return Err(Error::Denied)}
    let count=v.get("pin_count").filter(|v|!v.is_null()).map(|v|v.as_u64().ok_or_else(||invalid("pinterest.pin_count"))).transpose()?;
    Ok(Board {count,privacy:privacy.into()})
}
fn dimension(v:&Value,key:&str)->rss_contracts::Result<Option<u32>>{v.get(key).filter(|v|!v.is_null()).map(|v|v.as_u64().and_then(|n|u32::try_from(n).ok()).filter(|n|*n>0).ok_or_else(||invalid("pinterest.dimensions"))).transpose()}
fn duration(v:Option<&Value>)->rss_contracts::Result<Option<u64>> {
    let Some(v)=v.filter(|v|!v.is_null())else{return Ok(None);};
    if let Some(value)=v.as_u64(){return Ok(Some(value));}
    let value=v.as_f64().filter(|value|value.is_finite()&&*value>=0.0&&*value<18_446_744_073_709_551_616.0).ok_or_else(||invalid("pinterest.duration_ms"))?;
    if value.fract()!=0.0{return Ok(None);}
    Ok(Some(value as u64))
}
fn image(v:&Value,b:&Bounds)->rss_contracts::Result<MediaEvidence>{let url=text(v,"url")?.to_owned();public::public_url(&url,b)?;Ok(MediaEvidence {key:MemberKey {page_id:None,media_id:None},kind:hsscrap::MediaKind::Image,url,width:dimension(v,"width")?,height:dimension(v,"height")?,duration_ms:None})}
fn images(v:&Value,b:&Bounds)->rss_contracts::Result<MediaEvidence> {
    let map=v.as_object().ok_or_else(||invalid("pinterest.images"))?;if map.is_empty()||map.len()>b.max_collection_items{return Err(invalid("pinterest.images"));}
    let mut selected=None;for value in map.values(){let candidate=image(value,b)?;let area=|m:&MediaEvidence|u64::from(m.width.unwrap_or(0))*u64::from(m.height.unwrap_or(0));if selected.as_ref().is_none_or(|old|area(&candidate)>area(old)){selected=Some(candidate);}}
    selected.ok_or_else(||invalid("pinterest.images"))
}
fn media(v:&Value,b:&Bounds)->rss_contracts::Result<Vec<MediaEvidence>> {
    let kind=text(v,"media_type")?;
    match kind {
        "image"=>Ok(vec![images(&v["images"],b)?]),
        "video"=>{let url=text(v,"video_url")?.to_owned();public::public_url(&url,b)?;
            let duration_ms=duration(v.get("duration"))?;
            Ok(vec![MediaEvidence {key:MemberKey {page_id:None,media_id:None},kind:hsscrap::MediaKind::Video,url,width:dimension(v,"width")?,height:dimension(v,"height")?,duration_ms}])},
        "multiple_images"|"multiple_videos"|"multiple_mixed"=>{
            let items=v.get("items").and_then(Value::as_array).ok_or_else(||invalid("pinterest.media_items"))?;
            if items.is_empty()||items.len()>b.max_collection_items{return Err(invalid("pinterest.media_items"));}
            let mut result=vec![];for item in items {
                let item_kind=match kind {"multiple_images"=>"image","multiple_videos"=>"video",_=>text(item,"item_type")?};
                let mut values=if item_kind=="image"{vec![images(&item["images"],b)?]}else if item_kind=="video"{let mut item=item.clone();item["media_type"]=Value::String("video".into());media(&item,b)?}else{return Err(invalid("pinterest.unsupported_media"));};result.append(&mut values);
            }Ok(result)
        },_=>Err(invalid("pinterest.unsupported_media")),
    }
}
pub(super) fn page(bytes:&[u8],board_id:&str,max:usize,max_entries:usize,b:&Bounds)->rss_contracts::Result<Document> {
    let root=json(bytes,max)?;let items=root.get("items").and_then(Value::as_array).ok_or_else(||invalid("pinterest.items"))?;
    if items.len()>max_entries||items.len()>b.max_collection_items{return Err(Error::LimitExceeded {field:"pinterest.items".into()});}
    // The optional bookmark is absent/null only at a terminal provider document.
    let next=optional(&root,"bookmark")?;if let Some(next)=&next{token(next,b)?;}let hash=digest(bytes);let mut entries=vec![];let mut seen=BTreeSet::new();
    for item in items {
        let id=text(item,"id")?;if !public::numeric_id(id)||!seen.insert(id.to_owned())||text(item,"board_id")?!=board_id{return Err(invalid("pinterest.pin_identity"));}
        let published_at=optional(item,"created_at")?.map(|raw|crate::normalize::publication(&raw)).transpose()?;
        let entry=EntryEvidence {id:id.into(),title:optional(item,"title")?,description:optional(item,"description")?,author:None,published_at,outbound_url:optional(item,"link")?,media:media(&item["media"],b)?,document_digest:hash.clone()};entry.validate(b)?;entries.push(entry);
    }
    Ok(Document {entries,next,digest:hash})
}
