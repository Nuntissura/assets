use super::*;
use crate::EntryEvidence;
use serde_json::Value;
pub(super) struct Document {pub entries:Vec<EntryEvidence>,pub next:Option<String>,pub digest:String,pub terminal:bool}
pub(super) fn document(bytes:&[u8],mode:AccessMode,account:Option<&crate::normalize::Account>,max:usize,max_entries:usize,b:&Bounds)->rss_contracts::Result<Document>{
    let root=crate::normalize::json(bytes,max)?;crate::normalize::status(&root)?;
    let items=root.get("itemList").and_then(Value::as_array).ok_or_else(||invalid("tiktok.item_list"))?;
    if items.len()>max_entries||items.len()>b.max_collection_items{return Err(Error::LimitExceeded {field:"tiktok.items".into()});}
    let more=match root.get("hasMore"){Some(Value::Bool(v))=>*v,Some(Value::Number(v)) if v.as_u64()==Some(0)=>false,Some(Value::Number(v)) if v.as_u64()==Some(1)=>true,_=>return Err(invalid("tiktok.has_more"))};
    let next=if more&&mode!=AccessMode::HomeForYou{
        let n=crate::normalize::unsigned(root.get("cursor").ok_or_else(||invalid("tiktok.connected_cursor"))?)?;Some(n.to_string())
    }else{None};
    let hash=digest(bytes);let mut entries:Vec<EntryEvidence>=vec![];
    for item in items {
        let entry=crate::normalize::entry(item,&hash,b)?;
        if let Some(account)=account{if entry.author_id!=account.id||entry.author_sec_uid!=account.sec_uid||!entry.author.eq_ignore_ascii_case(&account.name){return Err(invalid("tiktok.source_author"));}}
        if let Some(prior)=entries.iter().find(|prior|prior.id==entry.id){if prior!=&entry{return Err(invalid("tiktok.repeated_item"));}}else{entries.push(entry);}
    }
    Ok(Document {entries,next,digest:hash,terminal:!more})
}
