use std::collections::{BTreeMap, BTreeSet};
use serde::{Deserialize, Serialize, Serializer, Deserializer};
use crate::{Error, Result, invalid};
mod records;
pub use records::*;

macro_rules! ids { ($($name:ident),+ $(,)?) => {$ (
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
    #[serde(transparent)]
    pub struct $name(pub uuid::Uuid);
    impl std::str::FromStr for $name {
        type Err = uuid::Error;
        fn from_str(s: &str) -> std::result::Result<Self, Self::Err> { s.parse().map(Self) }
    }
    impl std::fmt::Display for $name {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { self.0.fmt(f) }
    }
)+}; }
ids!(ItemId, CaptureId, ObservationId, MemberId, BindingId, MembershipId, PlacementId,
    AssignmentId, IdentityId, StreamId, ExportStreamId, OperationId);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct HostRef { pub host_instance_id: String, pub owner_kind: String, pub native_id: String }
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ScopeRef(pub String);
macro_rules! refs { ($($name:ident),+) => {$ (
    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
    #[serde(transparent)]
    pub struct $name(pub HostRef);
)+}; }
refs!(TagRef, ProjectRef, FolderRef, MediaRef, OccurrenceRef);
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberRef { pub item_id: ItemId, pub member_id: MemberId }

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Revision(pub u64);
impl Revision {
    pub fn next(self) -> Result<Self> { self.0.checked_add(1).map(Self).ok_or_else(|| invalid("revision", "overflow")) }
    pub fn validate(self) -> Result<()> { if self.0 == 0 { Err(invalid("revision", "must start at 1")) } else { Ok(()) } }
}
impl Serialize for Revision {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok,S::Error> { s.serialize_str(&self.0.to_string()) }
}
impl<'de> Deserialize<'de> for Revision {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self,D::Error> {
        let text = String::deserialize(d)?;
        if text.is_empty() || (text.len()>1 && text.starts_with('0')) || !text.bytes().all(|c| c.is_ascii_digit()) {
            return Err(serde::de::Error::custom("expected canonical unsigned decimal string"));
        }
        let n=text.parse::<u64>().map_err(serde::de::Error::custom)?;
        if n == 0 { return Err(serde::de::Error::custom("revision must start at 1")); }
        Ok(Self(n))
    }
}
pub mod optional_u64 {
    use super::*;
    pub fn serialize<S:Serializer>(value:&Option<u64>, s:S)->std::result::Result<S::Ok,S::Error>{value.map(|n|n.to_string()).serialize(s)}
    pub fn deserialize<'de,D:Deserializer<'de>>(d:D)->std::result::Result<Option<u64>,D::Error>{let value=Option::<String>::deserialize(d)?;
        value.map(|text| {
            if text.is_empty() || (text.len()>1 && text.starts_with('0')) || !text.bytes().all(|c|c.is_ascii_digit()) { return Err(serde::de::Error::custom("expected canonical unsigned decimal string")); }
            text.parse::<u64>().map_err(serde::de::Error::custom)
        }).transpose()}
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Instant(String);
impl Instant {
    pub fn as_str(&self)->&str { &self.0 }
    pub fn parse(value: &str)->Result<Self> { Self::try_from(value.to_owned()) }
}
impl TryFrom<String> for Instant {
    type Error = Error;
    fn try_from(s:String)->Result<Self> {
        if !s.ends_with('Z') || chrono::DateTime::parse_from_rfc3339(&s).is_err() { return Err(invalid("instant", "expected RFC3339 UTC Z")); }
        Ok(Self(s))
    }
}
impl From<Instant> for String { fn from(v:Instant)->Self { v.0 } }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag="mode", content="value", rename_all="snake_case")]
pub enum Override<T> { Inherit, Set(T), Clear }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag="mode", content="value", rename_all="snake_case")]
pub enum Patch<T> { Keep, Set(T), Clear }
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extensions {
    #[serde(default)] pub extensions: BTreeMap<String, serde_json::Value>,
    #[serde(default, deserialize_with="unique_features")] pub required_features: BTreeSet<String>,
}
impl Extensions {
    pub fn validate_bounded(&self,b:&Bounds,supported:&BTreeSet<String>)->Result<()> {
        self.validate(supported)?;
        fn value(v:&serde_json::Value,b:&Bounds)->Result<()> {
            match v {
                serde_json::Value::String(s)=>b.text("extensions",s,b.max_text_bytes)?,
                serde_json::Value::Array(a)=>{
                    if a.len()>b.max_collection_items { return Err(Error::LimitExceeded{field:"extensions".into(),limit:b.max_collection_items}); }
                    for v in a {value(v,b)?;}
                },
                serde_json::Value::Object(o)=>{
                    if o.len()>b.max_collection_items {return Err(Error::LimitExceeded{field:"extensions".into(),limit:b.max_collection_items});}
                    for (k,v) in o {b.text("extensions",k,b.max_text_bytes)?;value(v,b)?;}
                },
                _=>{}
            } Ok(())
        }
        if self.extensions.len()>b.max_collection_items || self.required_features.len()>b.max_collection_items {return Err(Error::LimitExceeded{field:"extensions".into(),limit:b.max_collection_items});}
        for (k,v) in &self.extensions { b.text("extensions",k,b.max_text_bytes)?;value(v,b)?; }
        for f in &self.required_features {b.text("required_features",f,b.max_text_bytes)?;}
        Ok(())
    }
    pub fn validate(&self, supported:&BTreeSet<String>)->Result<()> {
        if let Some(name)=self.extensions.keys().find(|k|!k.contains('.')) { return Err(invalid("extensions", &format!("namespace required: {name}"))); }
        for name in &self.required_features { if !supported.contains(name) { return Err(Error::UnsupportedRequiredFeature{feature:name.clone()}); } }
        Ok(())
    }
}
macro_rules! enums { ($name:ident: $($v:ident),+) => {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all="snake_case")]
    pub enum $name { $($v),+ }
}; }
enums!(Lifecycle: Active, Tombstoned);
enums!(MemberLifecycle: Staged, Active, Tombstoned);
enums!(CaptureKind: UserInput, SourceIngest, LegacyMigration);
enums!(AvailabilityState: Pending, Available, Unavailable, Failed, Restricted);
enums!(MediaKind: Image, Video, Audio, Document, Link, Unsupported);
enums!(RemoteRole: Original, Preview, Proxy, Embed, Permalink);
enums!(RetainedRole: Original, Preview, Proxy, Derived);
enums!(TagOrigin: User, Import, LegacyUnknown);
enums!(ImportState: Active, LocalTombstone, SourceDeleted, Revoked, Conflict);
enums!(OutcomeState: Pending, Committed, Rejected, Conflict);
enums!(Coverage: Complete, Partial);
enums!(PublicationPrecision: Year, Month, Day, Minute, Second, Subsecond);
enums!(Fit: Contain, Cover);
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Availability { pub state: AvailabilityState, #[serde(deserialize_with="required_option")] pub checked_at: Option<Instant>, #[serde(deserialize_with="required_option")] pub reason_code: Option<String>, #[serde(deserialize_with="required_option")] pub retry_after: Option<Instant> }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationTime { pub raw_value:String, pub precision:PublicationPrecision, pub local_value:String, #[serde(deserialize_with="required_option")] pub offset_minutes:Option<i16> }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GallerySnapshot { pub coverage:Coverage, pub members:Vec<MemberObservation>, #[serde(deserialize_with="required_option")] pub full_set_digest:Option<String> }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag="kind",content="value",rename_all="snake_case")]
pub enum RemoteTarget { PublicUrl(String), PrivateLocator(HostRef), Unavailable(String) }
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag="kind",content="value",rename_all="snake_case")]
pub enum TagTarget { Item(ItemId), Project(ProjectRef), Membership(MembershipId), Folder(FolderRef), Occurrence(OccurrenceRef) }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Presentation { #[serde(deserialize_with="required_option")] pub crop:Option<NormalizedRect>, #[serde(deserialize_with="required_option")] pub fit:Option<Fit>, #[serde(with="optional_u64")] pub frame_ms:Option<u64>, pub layout:Extensions }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NormalizedRect { pub x:f64, pub y:f64, pub width:f64, pub height:f64 }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedVersion { pub revision:Revision, pub payload_hash:String }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedRevision { pub owner:HostRef, pub revision:Revision }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedStreamVersion { pub stream_id:ExportStreamId, pub version:AcceptedVersion }
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bounds { pub max_url_bytes:usize, pub max_text_bytes:usize, pub max_collection_items:usize, pub max_page_items:usize, pub max_operation_bytes:usize }
impl Bounds {
    pub fn validate(&self)->Result<()> { if [self.max_url_bytes,self.max_text_bytes,self.max_collection_items,self.max_page_items,self.max_operation_bytes].contains(&0) { Err(invalid("bounds", "finite positive limits required")) } else { Ok(()) } }
    pub fn text(&self, field:&str, value:&str, limit:usize)->Result<()> { if value.len()>limit { Err(Error::LimitExceeded{field:field.into(),limit}) } else { Ok(()) } }
}

pub fn required_option<'de,D,T>(d:D)->std::result::Result<Option<T>,D::Error>
where D:Deserializer<'de>, T:Deserialize<'de> { Option::<T>::deserialize(d) }
fn unique_features<'de,D:Deserializer<'de>>(d:D)->std::result::Result<BTreeSet<String>,D::Error> {
    let values=Vec::<String>::deserialize(d)?; let n=values.len();
    let unique:BTreeSet<_>=values.into_iter().collect();
    if n!=unique.len() { return Err(serde::de::Error::custom("duplicate required feature")); } Ok(unique)
}
pub mod expected_revision_map {
    use super::*;
    fn key(r:&HostRef)->Vec<u8> { serde_json::to_vec(&serde_json::json!(r)).expect("HostRef contains only JSON strings") }
    pub fn serialize<S:Serializer>(v:&[ExpectedRevision],s:S)->std::result::Result<S::Ok,S::Error> {
        let mut entries:Vec<_>=v.iter().collect();entries.sort_by(|a,b|key(&a.owner).cmp(&key(&b.owner)));
        if entries.windows(2).any(|w|w[0].owner==w[1].owner) {return Err(serde::ser::Error::custom("duplicate revision map key"));} entries.serialize(s)
    }
    pub fn deserialize<'de,D:Deserializer<'de>>(d:D)->std::result::Result<Vec<ExpectedRevision>,D::Error> {
        let v=Vec::<ExpectedRevision>::deserialize(d)?;
        if v.windows(2).any(|w|key(&w[0].owner)>=key(&w[1].owner)) {return Err(serde::de::Error::custom("revision map keys must be sorted and unique"));} Ok(v)
    }
}
pub mod accepted_version_map {
    use super::*;
    pub fn serialize<S:Serializer>(v:&[AcceptedStreamVersion],s:S)->std::result::Result<S::Ok,S::Error> {
        let mut entries:Vec<_>=v.iter().collect();entries.sort_by_key(|v|v.stream_id);
        if entries.windows(2).any(|w|w[0].stream_id==w[1].stream_id) {return Err(serde::ser::Error::custom("duplicate accepted stream"));} entries.serialize(s)
    }
    pub fn deserialize<'de,D:Deserializer<'de>>(d:D)->std::result::Result<Vec<AcceptedStreamVersion>,D::Error> {
        let v=Vec::<AcceptedStreamVersion>::deserialize(d)?;
        if v.windows(2).any(|w|w[0].stream_id>=w[1].stream_id) {return Err(serde::de::Error::custom("stream map keys must be sorted and unique"));} Ok(v)
    }
}
