use hsscrap::*;
use std::collections::BTreeSet;
fn bounds() -> Bounds {
    Bounds { max_url_bytes:4096, max_text_bytes:10000, max_collection_items:1000, max_page_items:100, max_operation_bytes:100000 }
}
fn capture() -> serde_json::Value { serde_json::json!({
    "capture_id":"00000000-0000-0000-0000-000000000001", "item_id":"00000000-0000-0000-0000-000000000002",
    "capture_kind":"legacy_migration", "access_partition":"private", "captured_url":"https://example.org/a?q=1&q=2#f",
    "original_url":null,"identity_url":"https://example.org/a?q=1&q=2#f", "normalization_version":"hsscrap.url.v1",
    "captured_at":null,"created_here_at":"2026-10-23T00:00:00Z","actor_ref":null,
    "operation_id":"00000000-0000-0000-0000-000000000003", "extensions":{"vendor.keep":{"x":true}},"required_features":[]
}) }
#[test] fn legacy_history_and_unknown_optional_extensions_roundtrip() {
    let input=capture(); let value:URLCapture=decode(&input.to_string(),&bounds(),&BTreeSet::new()).unwrap();
    assert!(value.original_url.is_none()); assert!(value.captured_at.is_none());
    assert_eq!(serde_json::to_value(value).unwrap(),input);
}
#[test] fn input_capture_missing_history_is_rejected() {
    let mut input=capture(); input["capture_kind"]="user_input".into();
    assert!(decode::<URLCapture>(&input.to_string(),&bounds(),&BTreeSet::new()).is_err());
    input["capture_kind"]="source_ingest".into();
    assert!(decode::<URLCapture>(&input.to_string(),&bounds(),&BTreeSet::new()).is_err());
}
#[test] fn required_features_and_bounds_fail_closed() {
    let mut input=capture(); input["required_features"]=serde_json::json!(["vendor.required"]);
    assert!(matches!(decode::<URLCapture>(&input.to_string(),&bounds(),&BTreeSet::new()),Err(Error::UnsupportedRequiredFeature{..})));
    let mut b=bounds(); b.max_operation_bytes=10;
    assert!(matches!(decode::<URLCapture>(&input.to_string(),&b,&BTreeSet::new()),Err(Error::LimitExceeded{..})));
}
#[test] fn decimal_wire_rejects_numbers_and_noncanonical_strings() {
    let text=serde_json::to_string(&u64::MAX.to_string()).unwrap();
    assert_eq!(serde_json::to_string(&Revision(u64::MAX)).unwrap(),text);
    assert_eq!(serde_json::from_str::<Revision>(&text).unwrap(),Revision(u64::MAX));
    for bad in ["1", "\"01\"", "\"+1\"", "\"-1\"", "\"18446744073709551616\""] { assert!(serde_json::from_str::<Revision>(bad).is_err()); }
    assert!(Revision(0).validate().is_err()); assert!(Revision(u64::MAX).next().is_err());
}
#[test] fn tri_state_and_time_are_explicit() {
    assert_ne!(serde_json::to_value(Override::<String>::Clear).unwrap(),serde_json::to_value(Override::<String>::Inherit).unwrap());
    assert!(Instant::parse("2026-10-23T00:00:00Z").is_ok());
    assert!(Instant::parse("2026-10-23T00:00:00+00:00").is_err());
    let mut p=PublicationTime{raw_value:"2024-02".into(),precision:PublicationPrecision::Month,local_value:"2024-02".into(),offset_minutes:None};
    assert!(p.validate().is_ok()); p.local_value="2024-02-30".into(); assert!(p.validate().is_err());
}
#[test] fn malformed_urls_are_rejected() {
    for url in ["https://secret@example.org", "https://@example.org", "https://:443/path", "https://[broken/path", "https://example.com:bad"] {
        let mut input=capture(); input["captured_url"]=url.into();
        assert!(decode::<URLCapture>(&input.to_string(),&bounds(),&BTreeSet::new()).is_err(),"{url}");
    }
}
#[test] fn nested_text_and_label_collection_limits_are_enforced() {
    let observed=serde_json::json!({
        "observation_id":"00000000-0000-0000-0000-000000000001", "item_id":"00000000-0000-0000-0000-000000000002",
        "stream_id":"00000000-0000-0000-0000-000000000003", "sequence":"1", "baseline_observation_id":null,
        "capture_id":"00000000-0000-0000-0000-000000000004", "appearance_ref":null, "producer":"feed",
        "access_partition":"private", "observed_at":"2026-10-23T00:00:00Z", "source_revision":null,
        "canonical_url_assertion":null, "provider":null, "host_site":"example.org", "title":null,
        "description":null,"author":null,"published_at":null,"provider_labels":[],"source_payload_digest":null,
        "status":{"state":"available","checked_at":null,"reason_code":null,"retry_after":null},
        "gallery_snapshot":{"coverage":"partial","full_set_digest":null,"members":[{
            "member_id":"00000000-0000-0000-0000-000000000005","producer_member_key":null,"media_kind":"image",
            "caption":"x","observed_mime":null,"observed_width":null,"observed_height":null,"observed_duration_ms":"0",
            "availability":{"state":"available","checked_at":null,"reason_code":null,"retry_after":null}, "remote_reference_ids":[]
        }]}
    });
    let mut value=observed.clone();
    assert!(decode::<MetadataObservation>(&value.to_string(),&bounds(),&BTreeSet::new()).is_ok());
    value["gallery_snapshot"]["members"][0]["caption"]="x".repeat(10001).into();
    assert!(matches!(decode::<MetadataObservation>(&value.to_string(),&bounds(),&BTreeSet::new()),Err(Error::LimitExceeded{field,..}) if field=="member.caption"));
    value=observed; value["provider_labels"]=serde_json::json!(["one","two"]);
    let mut b=bounds(); b.max_collection_items=1;
    assert!(matches!(decode::<MetadataObservation>(&value.to_string(),&b,&BTreeSet::new()),Err(Error::LimitExceeded{field,..}) if field=="provider_labels"));
}
#[test] fn nullable_snapshots_require_keys_and_revisions_start_at_one() {
    let mut v=capture(); v.as_object_mut().unwrap().remove("original_url");
    assert!(decode::<URLCapture>(&v.to_string(),&bounds(),&BTreeSet::new()).is_err());
    assert!(serde_json::from_str::<Revision>("\"0\"").is_err());
    let availability=serde_json::json!({"state":"available","reason_code":null,"retry_after":null});
    assert!(serde_json::from_value::<Availability>(availability).is_err());
}
#[test] fn extension_bounds_and_unique_features_are_enforced() {
    let mut v=capture(); v["extensions"]["vendor.keep"]=serde_json::json!({"nested":"x".repeat(10001)});
    assert!(matches!(decode::<URLCapture>(&v.to_string(),&bounds(),&BTreeSet::new()),Err(Error::LimitExceeded{field,..}) if field=="extensions"));
    v=capture();v["required_features"]=serde_json::json!(["vendor.feature","vendor.feature"]);
    assert!(serde_json::from_value::<URLCapture>(v).is_err());
}
#[test] fn structured_map_serialization_sorts_and_rejects_duplicates() {
    let a=HostRef{host_instance_id:"host".into(),owner_kind:"items".into(),native_id:"a".into()};
    let b=HostRef{native_id:"b".into(),..a.clone()};
    let mut c=hsscrap::ports::CommandContext{
        actor:a.clone(),scope:ScopeRef("private".into()),operation_id:OperationId(uuid::Uuid::nil()),intent_digest:"intent".into(),bounds:bounds(),
        expected_revisions:vec![ExpectedRevision{owner:b,revision:Revision(2)},ExpectedRevision{owner:a.clone(),revision:Revision(1)}]
    };
    let value=serde_json::to_value(&c).unwrap();
    assert_eq!(value["expected_revisions"][0]["owner"]["native_id"],"a");
    assert!(serde_json::from_value::<hsscrap::ports::CommandContext>(value.clone()).is_ok());
    let mut unsorted=value;unsorted["expected_revisions"].as_array_mut().unwrap().reverse();
    assert!(serde_json::from_value::<hsscrap::ports::CommandContext>(unsorted).is_err());
    c.expected_revisions[0].owner=a;
    assert!(serde_json::to_value(&c).is_err());
}
#[test] fn override_patch_rejects_empty_set() {
    let patch=hsscrap::ports::ItemPatch{
        title:Patch::Set(Override::Set(String::new())),description:Patch::Keep,cover:Patch::Keep,
        selected_observation:Patch::Keep,gallery_observation:Patch::Keep,
    };
    assert!(patch.validate(&bounds(),&BTreeSet::new()).is_err());
}
#[test] fn member_extensions_are_preserved() {
    let value=serde_json::json!({
        "member_id":"00000000-0000-0000-0000-000000000005","producer_member_key":null,"media_kind":"image",
        "caption":null,"observed_mime":null,"observed_width":null,"observed_height":null,"observed_duration_ms":"0",
        "availability":{"state":"available","checked_at":null,"reason_code":null,"retry_after":null},"remote_reference_ids":[],
        "extensions":{"vendor.future":{"keep":true}},"required_features":[]
    });
    let member:MemberObservation=serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(member).unwrap(),value);
}
