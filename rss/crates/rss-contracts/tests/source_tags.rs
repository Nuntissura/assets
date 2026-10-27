use rss_contracts::*;
use serde_json::json;

fn bounds() -> Bounds { Bounds { max_url_bytes:4096, max_text_bytes:4096, max_collection_items:100, max_page_items:25, max_operation_bytes:100000 } }
fn tag(id: &str) -> TagRef { TagRef(HostRef { host_instance_id:"catalogue".into(), owner_kind:"shared-tag".into(), native_id:id.into() }) }
fn source() -> Source { Source { source_ref:tag("source").0, owner_scope:ScopeRef("account".into()), provider:Provider::Syndication,
    original_url:"https://example.test/feed".into(), display_name:"Feed".into(), revision:Revision(1), paused:false, common:Extensions::default() } }

#[test]
fn old_source_bytes_and_unrelated_extensions_roundtrip_without_tag_insertion() {
    let mut source = source();
    source.common.extensions.insert("portal.connection_selection".into(), json!({"connection_id":"kept","unknown":[1,{"future":true}]}));
    let bytes = serde_json::to_vec(&source).unwrap();
    let decoded: Source = serde_json::from_slice(&bytes).unwrap();
    assert!(decoded.source_tags(&bounds()).unwrap().tag_refs.is_empty());
    assert_eq!(serde_json::to_vec(&decoded).unwrap(), bytes);
    assert!(!decoded.common.extensions.contains_key(SOURCE_TAGS_EXTENSION));
}

#[test]
fn source_tag_extension_is_strict_bounded_and_host_neutral() {
    let mut source = source();
    let tags = SourceTags::new((0..50).map(|id| tag(&id.to_string())).collect(), &bounds()).unwrap();
    source.common.extensions.insert(SOURCE_TAGS_EXTENSION.into(), json!(tags));
    assert_eq!(source.source_tags(&bounds()).unwrap(), tags);
    assert!(SourceTags::new((0..51).map(|id| tag(&id.to_string())).collect(), &bounds()).is_err());
    let mut smaller = bounds(); smaller.max_collection_items=1;
    assert!(SourceTags::new(vec![tag("one"),tag("two")], &smaller).is_err());
    let reference = json!(tag("one"));
    let mut extra_reference = reference.clone(); extra_reference["unknown"] = json!(true);
    let mut empty_reference = reference.clone(); empty_reference["native_id"] = json!("");
    let mut oversized_reference = reference.clone(); oversized_reference["native_id"] = json!("x".repeat(4097));
    for malformed in [json!(null), json!({"version":2,"tag_refs":[]}), json!({"version":1,"tag_refs":[],"future":1}),
        json!({"version":1,"tag_refs":[reference.clone(),reference]}),json!({"version":1,"tag_refs":[extra_reference]}),
        json!({"version":1,"tag_refs":[empty_reference]}),json!({"version":1,"tag_refs":[oversized_reference]}),
        json!({"version":1}),json!({"version":1,"tag_refs":"not a list"})] {
        source.common.extensions.insert(SOURCE_TAGS_EXTENSION.into(), malformed.clone());
        let before = serde_json::to_vec(&source).unwrap();
        assert!(source.source_tags(&bounds()).is_err(), "Accepted malformed source tags: {malformed}");
        assert_eq!(serde_json::to_vec(&source).unwrap(), before);
    }
}
