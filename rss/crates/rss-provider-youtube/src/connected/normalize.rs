use super::*;
use serde_json::Value;
use crate::{EntryEvidence, Preview};
pub(super) struct Document {pub entries:Vec<EntryEvidence>,pub channels:Vec<ChannelEvidence>,pub next:Option<String>,pub digest:String,pub verified_empty:bool,pub omitted:bool}
fn text<'a>(v:&'a Value,key:&str)->rss_contracts::Result<&'a str> {v.get(key).and_then(Value::as_str).ok_or_else(||invalid("youtube.connected_document"))}
fn next(root:&Value,b:&Bounds)->rss_contracts::Result<Option<String>> {root.get("nextPageToken").map(|v|{let t=v.as_str().ok_or_else(||invalid("youtube.next"))?;bounded_token(t,b)?;Ok(t.to_owned())}).transpose()}
fn title(v:&Value)->Option<String> {v.get("simpleText").and_then(Value::as_str).map(str::to_owned).or_else(||v.get("runs")?.as_array()?.iter().map(|v|v.get("text")?.as_str()).collect::<Option<Vec<_>>>().map(|v|v.concat()))}
pub(super) fn api(bytes:&[u8],resource:&str,target:&str,own:Option<&str>,max:usize,b:&Bounds)->rss_contracts::Result<Document> {
    let root=crate::normalize::json(bytes,max)?;
    if root.get("error").is_some(){return Err(Error::Denied);}
    let kind=format!("youtube#{resource}ListResponse");if text(&root,"kind")?!=kind{return Err(invalid("youtube.resource_kind"));}
    let items=root.get("items").and_then(Value::as_array).ok_or_else(||invalid("youtube.items"))?;
    if items.len()>b.max_collection_items{return Err(Error::LimitExceeded {field:"youtube.items".into()});}
    let next=next(&root,b)?;let hash=digest(bytes);let mut entries=vec![];let mut channels=vec![];let mut seen=BTreeSet::new();
    for item in items {
        let snippet=&item["snippet"];
        if resource=="subscription" {
            if text(item,"kind")?!="youtube#subscription"||text(&snippet["resourceId"],"kind")?!="youtube#channel" {return Err(invalid("youtube.subscription"));}
            let channel=text(&snippet["resourceId"],"channelId")?;if !crate::public::channel_id(channel)||!seen.insert(channel.to_owned()){return Err(invalid("youtube.subscription_identity"));}
            if let Some(expected)=own {if text(snippet,"channelId")?!=expected{return Err(Error::Denied);}}
            let value=snippet.get("title").map(|v|v.as_str().map(str::to_owned).ok_or_else(||invalid("youtube.channel_title"))).transpose()?;
            if let Some(v)=&value{b.text("youtube.channel_title",v,b.max_text_bytes)?;}
            channels.push(ChannelEvidence {channel_id:channel.into(),title:value,document_digest:hash.clone()});continue;
        }
        let (id,channel,date)=if resource=="playlistItem" {
            if text(item,"kind")?!="youtube#playlistItem"||text(snippet,"playlistId")?!=target||text(&snippet["resourceId"],"kind")?!="youtube#video" {return Err(invalid("youtube.playlist"));}
            let id=text(&snippet["resourceId"],"videoId")?;
            if text(&item["contentDetails"],"videoId")?!=id{return Err(invalid("youtube.video_identity"));}
            (id,text(snippet,"videoOwnerChannelId")?,item["contentDetails"].get("videoPublishedAt"))
        } else {
            if text(item,"kind")?!="youtube#video"||text(item,"id")?!=target{return Err(invalid("youtube.video_identity"));}
            (text(item,"id")?,text(snippet,"channelId")?,snippet.get("publishedAt"))
        };
        if own.is_some_and(|v|v!=channel){return Err(Error::Denied);}
        if !matches!(text(&item["status"],"privacyStatus")?,"public"|"private"|"unlisted"){return Err(Error::Denied);}
        if !seen.insert(id.to_owned()){return Err(invalid("youtube.repeated_video"));}
        let previews=if let Some(map)=snippet.get("thumbnails").and_then(Value::as_object) {
            if map.len()>b.max_collection_items{return Err(invalid("youtube.previews"));}
            map.values().map(|v|Ok(Preview {url:text(v,"url")?.into(),width:None,height:None})).collect::<rss_contracts::Result<Vec<_>>>()?
        }else{vec![]};
        let entry=EntryEvidence {video_id:id.into(),channel_id:channel.into(),title:Some(text(snippet,"title")?.into()),description:snippet.get("description").and_then(Value::as_str).map(str::to_owned),author:snippet.get("channelTitle").and_then(Value::as_str).map(str::to_owned),
            published_at:date.map(|v|crate::normalize::publication(v.as_str().ok_or_else(||invalid("youtube.publication"))?)).transpose()?,previews,document_digest:hash.clone()};
        entry.validate(b)?;entries.push(entry);
    }
    let total=root.get("pageInfo").and_then(|v|v.get("totalResults")).and_then(Value::as_u64).ok_or_else(||invalid("youtube.page_info"))?;
    if total<(items.len() as u64)||items.is_empty()&&next.is_some(){return Err(invalid("youtube.page_info"));}
    let verified_empty=items.is_empty()&&next.is_none()&&total==0&&resource!="video";
    if items.is_empty()&&!verified_empty{return Err(Error::NotFound);}
    Ok(Document {entries,channels,next,digest:hash,verified_empty,omitted:false})
}

pub(super) fn browse(bytes:&[u8],expected:&str,continued:bool,max:usize,b:&Bounds)->rss_contracts::Result<Document> {
    let root=crate::normalize::json(bytes,max)?;
    if root.get("error").is_some(){return Err(Error::Denied);}
    let mut queue=std::collections::VecDeque::new();
    if continued {
        let actions=root.get("onResponseReceivedActions").or_else(||root.get("onResponseReceivedEndpoints")).and_then(Value::as_array).ok_or_else(||invalid("youtube.browse_continuation"))?;
        if actions.len()>b.max_collection_items{return Err(invalid("youtube.browse_actions"));}
        for action in actions {if let Some(items)=action.get("appendContinuationItemsAction").and_then(|v|v.get("continuationItems")){push(&mut queue,items,b)?;}else{return Err(invalid("youtube.browse_action"));}}
    }else{
        let tabs=root.pointer("/contents/twoColumnBrowseResultsRenderer/tabs").and_then(Value::as_array).ok_or_else(||invalid("youtube.browse_tabs"))?;
        if tabs.len()>b.max_collection_items{return Err(invalid("youtube.browse_tabs"));}
        let selected=tabs.iter().filter_map(|v|v.get("tabRenderer")).filter(|v|v.get("selected")==Some(&Value::Bool(true))).collect::<Vec<_>>();
        if selected.len()!=1{return Err(invalid("youtube.browse_selection"));}let tab=selected[0];
        let identity=tab.get("tabIdentifier").and_then(Value::as_str).or_else(||tab.pointer("/endpoint/browseEndpoint/browseId").and_then(Value::as_str));
        if identity!=Some(expected){return Err(Error::StaleResult);}
        queue.push_back(tab.get("content").ok_or_else(||invalid("youtube.browse_content"))?);
    }
    let hash=digest(bytes);let mut result=Document {entries:vec![],channels:vec![],next:None,digest:hash.clone(),verified_empty:false,omitted:false};let mut visited=0;
    while let Some(node)=queue.pop_front() {
        visited+=1;if visited>b.max_collection_items{return Err(Error::LimitExceeded {field:"youtube.browse_nodes".into()});}
        let object=node.as_object().ok_or_else(||invalid("youtube.browse_node"))?;
        if object.len()!=1{result.omitted=true;continue;}
        let (kind,value)=object.iter().next().ok_or_else(||invalid("youtube.browse_node"))?;
        match kind.as_str() {
            "richGridRenderer"|"sectionListRenderer"|"itemSectionRenderer"|"shelfRenderer"=>push(&mut queue,value.get("contents").or_else(||value.get("content")).ok_or_else(||invalid("youtube.browse_container"))?,b)?,
            "gridRenderer"=>push(&mut queue,value.get("items").ok_or_else(||invalid("youtube.browse_grid"))?,b)?,
            "richItemRenderer"=>push(&mut queue,value.get("content").ok_or_else(||invalid("youtube.browse_rich_item"))?,b)?,
            "lockupViewModel"=>{
                if let Some(entry)=lockup(value,&hash,b)? {
                    if result.entries.len()>=b.max_collection_items{return Err(invalid("youtube.browse_entries"));}
                    if let Some(prior)=result.entries.iter().find(|e|e.video_id==entry.video_id){if prior!=&entry{return Err(invalid("youtube.repeated_video"));}}else{result.entries.push(entry);}
                }else{result.omitted=true;}
            },
            "videoRenderer"|"gridVideoRenderer"|"playlistVideoRenderer"=>{
                if result.entries.len()>=b.max_collection_items{return Err(invalid("youtube.browse_entries"));}
                let id=text(value,"videoId")?;let owner=value.get("ownerText").or_else(||value.get("shortBylineText")).and_then(|v|v.get("runs")).and_then(Value::as_array).and_then(|v|v.first()).ok_or_else(||invalid("youtube.browse_owner"))?;
                let channel=owner.pointer("/navigationEndpoint/browseEndpoint/browseId").and_then(Value::as_str).ok_or_else(||invalid("youtube.browse_channel"))?;
                let previews=value.pointer("/thumbnail/thumbnails").and_then(Value::as_array).ok_or_else(||invalid("youtube.browse_thumbnail"))?;
                if previews.len()>b.max_collection_items{return Err(invalid("youtube.previews"));}
                let entry=EntryEvidence {video_id:id.into(),channel_id:channel.into(),title:title(&value["title"]),description:None,author:owner.get("text").and_then(Value::as_str).map(str::to_owned),published_at:None,
                    previews:previews.iter().map(|v|Ok(Preview {url:text(v,"url")?.into(),width:None,height:None})).collect::<rss_contracts::Result<_>>()?,document_digest:hash.clone()};
                entry.validate(b)?;
                if let Some(prior)=result.entries.iter().find(|e|e.video_id==entry.video_id){if prior!=&entry{return Err(invalid("youtube.repeated_video"));}}else{result.entries.push(entry);}
            },
            "continuationItemRenderer"=>{
                let token=value.pointer("/continuationEndpoint/continuationCommand/token").and_then(Value::as_str).ok_or_else(||invalid("youtube.browse_token"))?;bounded_token(token,b)?;
                if result.next.replace(token.into()).is_some(){return Err(invalid("youtube.browse_multiple_continuations"));}
            },
            _=>result.omitted=true,
        }
    }
    if result.entries.is_empty()&&result.next.is_none(){return Err(invalid("youtube.browse_empty_unverified"));}
    Ok(result)
}
fn push<'a>(queue:&mut std::collections::VecDeque<&'a Value>,value:&'a Value,b:&Bounds)->rss_contracts::Result<()> {
    let added=value.as_array().map_or(1,Vec::len);if queue.len().saturating_add(added)>b.max_collection_items{return Err(Error::LimitExceeded {field:"youtube.browse_queue".into()});}
    if let Some(items)=value.as_array(){queue.extend(items);}else{queue.push_back(value);}Ok(())
}
fn lockup(v:&Value,hash:&str,b:&Bounds)->rss_contracts::Result<Option<EntryEvidence>> {
    if v.get("contentType").and_then(Value::as_str)!=Some("LOCKUP_CONTENT_TYPE_VIDEO"){return Ok(None);}
    let id=text(v,"contentId")?;if !crate::public::video_id(id){return Err(invalid("youtube.lockup_video"));}
    let metadata=&v["metadata"]["lockupMetadataViewModel"];
    let rows=metadata.pointer("/metadata/contentMetadataViewModel/metadataRows").and_then(Value::as_array).ok_or_else(||invalid("youtube.lockup_metadata"))?;
    if rows.len()>b.max_collection_items{return Err(invalid("youtube.lockup_metadata"));}
    let mut channel=None;let mut count=0;
    for row in rows {
        let parts=row.get("metadataParts").and_then(Value::as_array).ok_or_else(||invalid("youtube.lockup_metadata"))?;
        count+=parts.len();if count>b.max_collection_items{return Err(invalid("youtube.lockup_metadata"));}
        for part in parts {if let Some(runs)=part.pointer("/text/commandRuns").and_then(Value::as_array) {
            if runs.len()>b.max_collection_items{return Err(invalid("youtube.lockup_commands"));}
            for run in runs {
                let tap=&run["onTap"];let command=tap.get("innertubeCommand").or_else(||tap.get("command")).unwrap_or(tap);
                if let Some(id)=command.pointer("/browseEndpoint/browseId").and_then(Value::as_str).filter(|id|crate::public::channel_id(id)) {
                    if channel.replace(id).is_some_and(|old|old!=id){return Err(invalid("youtube.lockup_channel"));}
                }
            }
        }}
    }
    let Some(channel)=channel else{return Ok(None);};
    let sources=v.pointer("/contentImage/thumbnailViewModel/image/sources").and_then(Value::as_array).ok_or_else(||invalid("youtube.lockup_thumbnail"))?;
    if sources.len()>b.max_collection_items{return Err(invalid("youtube.previews"));}
    let entry=EntryEvidence {video_id:id.into(),channel_id:channel.into(),title:metadata.pointer("/title/content").and_then(Value::as_str).map(str::to_owned),description:None,author:None,published_at:None,
        previews:sources.iter().map(|v|Ok(Preview {url:text(v,"url")?.into(),width:None,height:None})).collect::<rss_contracts::Result<_>>()?,document_digest:hash.into()};
    entry.validate(b)?;Ok(Some(entry))
}
