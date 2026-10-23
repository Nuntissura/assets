use std::collections::BTreeSet;
use serde::de::DeserializeOwned;
use crate::*;

pub trait Validate { fn validate(&self, bounds:&Bounds, supported:&BTreeSet<String>)->Result<()>; }
fn text(b:&Bounds,field:&str,value:&str)->Result<()> { b.text(field,value,b.max_text_bytes) }
fn host_ref(b:&Bounds,field:&str,value:&HostRef)->Result<()> {
    text(b,field,&value.host_instance_id)?; text(b,field,&value.owner_kind)?; text(b,field,&value.native_id)
}
fn availability(b:&Bounds,value:&Availability)->Result<()> {
    if let Some(reason)=&value.reason_code { text(b,"reason_code",reason)?; } Ok(())
}
pub fn decode<T: DeserializeOwned + Validate>(input:&str, bounds:&Bounds, supported:&BTreeSet<String>)->Result<T> {
    bounds.validate()?;
    bounds.text("operation", input, bounds.max_operation_bytes)?;
    let value:T=serde_json::from_str(input).map_err(|e|invalid("json", &e.to_string()))?;
    value.validate(bounds, supported)?;
    Ok(value)
}
pub(crate) fn http_url(value:&str)->bool {
    if value.chars().any(|c|c.is_control() || c.is_whitespace()) || !value.contains("://") { return false; }
    if value.split_once("://").is_some_and(|(_,tail)|tail.split(['/', '?', '#']).next().unwrap_or("").contains('@')) { return false; }
    let Ok(parsed)=url::Url::parse(value) else { return false; };
    matches!(parsed.scheme(),"http"|"https") && parsed.host_str().is_some() && parsed.username().is_empty() && parsed.password().is_none()
}
impl Validate for URLCapture {
    fn validate(&self,b:&Bounds,s:&BTreeSet<String>)->Result<()> {
        self.common.validate_bounded(b,s)?;
        text(b,"access_partition",&self.access_partition.0)?;
        text(b,"normalization_version",&self.normalization_version)?;
        if let Some(actor)=&self.actor_ref { host_ref(b,"actor_ref",actor)?; }
        for (name,url) in [("captured_url",&self.captured_url),("identity_url",&self.identity_url)] {
            b.text(name,url,b.max_url_bytes)?;
            if !http_url(url) { return Err(invalid(name,"absolute http(s) URL without userinfo required")); }
        }
        if let Some(url)=&self.original_url { b.text("original_url",url,b.max_url_bytes)?; if !http_url(url) {return Err(invalid("original_url","invalid URL"));} }
        if self.capture_kind == CaptureKind::UserInput && (self.original_url.is_none() || self.captured_at.is_none() || self.actor_ref.is_none()) { return Err(invalid("capture","user input requires original URL, capture instant and actor")); }
        if self.capture_kind == CaptureKind::SourceIngest && self.captured_at.is_none() { return Err(invalid("captured_at","source ingest requires actual instant")); }
        if self.normalization_version.is_empty() { return Err(invalid("normalization_version","required")); }
        Ok(())
    }
}
impl PublicationTime {
    pub fn validate(&self)->Result<()> {
        use chrono::{NaiveDate, NaiveDateTime};
        let v=&self.local_value;
        let valid=match self.precision {
            PublicationPrecision::Year => v.len()==4 && NaiveDate::parse_from_str(&format!("{v}-01-01"),"%Y-%m-%d").is_ok(),
            PublicationPrecision::Month => v.len()==7 && NaiveDate::parse_from_str(&format!("{v}-01"),"%Y-%m-%d").is_ok(),
            PublicationPrecision::Day => v.len()==10 && NaiveDate::parse_from_str(v,"%Y-%m-%d").is_ok(),
            PublicationPrecision::Minute => v.len()==16 && NaiveDateTime::parse_from_str(&format!("{v}:00"),"%Y-%m-%dT%H:%M:%S").is_ok(),
            PublicationPrecision::Second => v.len()==19 && NaiveDateTime::parse_from_str(v,"%Y-%m-%dT%H:%M:%S").is_ok(),
            PublicationPrecision::Subsecond => v.len()>20 && v.as_bytes().get(19)==Some(&b'.') && NaiveDateTime::parse_from_str(v,"%Y-%m-%dT%H:%M:%S%.f").is_ok(),
        };
        if !valid || self.raw_value.is_empty() || self.offset_minutes.is_some_and(|n|!(-1439..=1439).contains(&n)) { return Err(invalid("published_at","value does not match precision or offset")); }
        Ok(())
    }
}
fn override_text(value:&Override<String>,b:&Bounds,field:&str)->Result<()> {
    if let Override::Set(text)=value { if text.is_empty() { return Err(invalid(field,"set empty is invalid; use clear")); } b.text(field,text,b.max_text_bytes)?; } Ok(())
}
impl Validate for Item {
    fn validate(&self,b:&Bounds,s:&BTreeSet<String>)->Result<()> {
        self.common.validate_bounded(b,s)?; self.revision.validate()?;
        text(b,"owner_scope",&self.owner_scope.0)?;
        override_text(&self.title_override,b,"title_override")?; override_text(&self.description_override,b,"description_override")?;
        if self.member_order.len()>b.max_collection_items { return Err(Error::LimitExceeded{field:"member_order".into(),limit:b.max_collection_items}); }
        let unique:BTreeSet<_>=self.member_order.iter().collect();
        if unique.len()!=self.member_order.len() { return Err(invalid("member_order","duplicate member")); }
        if let Override::Set(member)=self.cover_override { if !unique.contains(&member) { return Err(invalid("cover_override","member is not active")); } }
        if (self.lifecycle==Lifecycle::Tombstoned)!=self.deleted_at.is_some() { return Err(invalid("lifecycle","tombstone and deleted_at must agree")); }
        Ok(())
    }
}
impl Validate for MetadataObservation {
    fn validate(&self,b:&Bounds,s:&BTreeSet<String>)->Result<()> {
        self.common.validate_bounded(b,s)?; self.sequence.validate()?;
        text(b,"access_partition",&self.access_partition.0)?;
        text(b,"producer",&self.producer)?; text(b,"host_site",&self.host_site)?;
        for (name,value) in [("provider",&self.provider),("source_revision",&self.source_revision),("source_payload_digest",&self.source_payload_digest)] {
            if let Some(value)=value { text(b,name,value)?; }
        }
        if let Some(reference)=&self.appearance_ref { host_ref(b,"appearance_ref",reference)?; }
        availability(b,&self.status)?;
        if self.provider_labels.len()>b.max_collection_items { return Err(Error::LimitExceeded{field:"provider_labels".into(),limit:b.max_collection_items}); }
        for label in &self.provider_labels { text(b,"provider_labels",label)?; }
        if self.capture_id.is_none() && self.appearance_ref.is_none() { return Err(invalid("observation","capture or appearance required")); }
        if let Some(p)=&self.published_at { p.validate()?; text(b,"published_at.raw_value",&p.raw_value)?; text(b,"published_at.local_value",&p.local_value)?; }
        if let Some(url)=&self.canonical_url_assertion {
            b.text("canonical_url_assertion",url,b.max_url_bytes)?;
            if !http_url(url) { return Err(invalid("canonical_url_assertion","invalid URL")); }
        }
        for (name,value) in [("title",&self.title),("description",&self.description),("author",&self.author)] { if let Some(v)=value { b.text(name,v,b.max_text_bytes)?; } }
        if let Some(g)=&self.gallery_snapshot {
            if g.members.len()>b.max_collection_items { return Err(Error::LimitExceeded{field:"gallery_snapshot".into(),limit:b.max_collection_items}); }
            if (g.coverage==Coverage::Complete)!=g.full_set_digest.is_some() { return Err(invalid("gallery_snapshot","complete requires digest; partial forbids digest")); }
            if let Some(digest)=&g.full_set_digest { text(b,"full_set_digest",digest)?; }
            for member in &g.members {
                member.common.validate_bounded(b,s)?;
                for (name,value) in [("member.caption",&member.caption),("member.observed_mime",&member.observed_mime),("member.producer_member_key",&member.producer_member_key)] {
                    if let Some(value)=value { text(b,name,value)?; }
                }
                availability(b,&member.availability)?;
                if member.remote_reference_ids.len()>b.max_collection_items { return Err(Error::LimitExceeded{field:"remote_reference_ids".into(),limit:b.max_collection_items}); }
                for reference in &member.remote_reference_ids { host_ref(b,"remote_reference_ids",reference)?; }
            }
            let unique:BTreeSet<_>=g.members.iter().map(|m|m.member_id).collect();
            if unique.len()!=g.members.len() { return Err(invalid("gallery_snapshot","duplicate member")); }
        }
        Ok(())
    }
}
impl Validate for crate::ports::ItemPatch {
    fn validate(&self,b:&Bounds,_:&BTreeSet<String>)->Result<()> {
        if let Patch::Set(v)=&self.title {override_text(v,b,"title")?;}
        if let Patch::Set(v)=&self.description {override_text(v,b,"description")?;}
        Ok(())
    }
}
