use super::*;
use hsscrap::transfer::{plan_export, TransferRecord};
use hsscrap::{CaptureId, Coverage, ExportStreamId, Item, Lifecycle, Override};
use std::collections::BTreeMap;

fn reference(id: &str) -> HostRef {
    HostRef {
        host_instance_id: "origin".into(),
        owner_kind: "rss".into(),
        native_id: id.into(),
    }
}
fn bounds() -> Bounds {
    Bounds {
        max_url_bytes: 4096,
        max_text_bytes: 4096,
        max_collection_items: 100,
        max_page_items: 25,
        max_operation_bytes: 100_000,
    }
}
fn supported() -> BTreeSet<String> {
    BTreeSet::from([REQUIRED_FEATURE.into()])
}
fn fixture() -> (TransferFields, ItemExtension, BTreeSet<OriginRef>) {
    let item_id: ItemId = "00000000-0000-0000-0000-000000000001".parse().unwrap();
    let capture_id: CaptureId = "00000000-0000-0000-0000-000000000002".parse().unwrap();
    let observation_id: ObservationId = "00000000-0000-0000-0000-000000000003".parse().unwrap();
    let at = Instant::parse("2026-10-25T00:00:00Z").unwrap();
    let tag = TagRef(reference("tag"));
    let origin_scope = ScopeRef("origin-profile".into());
    let fields = TransferFields {
        model_version: hsscrap::transfer::MODEL_VERSION.into(),
        export_stream_id: "00000000-0000-0000-0000-000000000004"
            .parse::<ExportStreamId>()
            .unwrap(),
        origin_instance_id: "origin".into(),
        origin_kind: "item".into(),
        origin_id: item_id.to_string(),
        revision: Revision(1),
        projection_policy_id: "reviewed-export.v1".into(),
        audience_ref: ScopeRef("destination-audience".into()),
        record: TransferRecord::Item(Item {
            item_id,
            owner_scope: ScopeRef("origin-account".into()),
            revision: Revision(1),
            first_capture_id: capture_id,
            selected_observation_id: None,
            gallery_observation_id: None,
            member_order: vec![],
            title_override: Override::Inherit,
            description_override: Override::Inherit,
            cover_override: Override::Inherit,
            created_at: at.clone(),
            updated_at: at.clone(),
            lifecycle: Lifecycle::Active,
            deleted_at: None,
            common: Extensions::default(),
        }),
        required_features: BTreeSet::new(),
        extensions: BTreeMap::new(),
    };
    let extension = ItemExtension {
        origin_scope: origin_scope.clone(),
        coverage: Coverage::Partial,
        sources: vec![SourceRevision {
            source_ref: reference("source"),
            revision: Revision(9_007_199_254_740_993),
        }],
        appearances: vec![SourceAppearance {
            appearance_ref: reference("appearance"),
            item_id,
            source_ref: reference("source"),
            access_partition: origin_scope.clone(),
            provider_entry_key: "entry".into(),
            first_seen_at: Some(at.clone()),
            last_seen_at: Some(at),
            latest_observation_id: Some(observation_id),
            source_state: SourceState::Active,
            common: Extensions::default(),
        }],
        states: vec![AppearanceState {
            state_ref: reference("state"),
            item_id,
            appearance_ref: reference("appearance"),
            legacy_item_ref: Some(reference("legacy")),
            scope: origin_scope,
            read: true,
            bookmarked: false,
            legacy_tag_refs: vec![tag.clone()],
            revision: Revision(2),
            common: Extensions::default(),
        }],
    };
    let origins = BTreeSet::from([
        OriginRef::Item(item_id),
        OriginRef::Capture(capture_id),
        OriginRef::Observation(observation_id),
        OriginRef::Tag(tag),
    ]);
    (fields, extension, origins)
}
fn context<'a>(
    scope: &'a ScopeRef,
    origins: &'a BTreeSet<OriginRef>,
    keys: &'a BTreeSet<String>,
    records: &'a [TransferRecord],
) -> ExtensionContext<'a> {
    ExtensionContext {
        origin_scope: scope,
        exported_origins: origins,
        allowed_extension_keys: keys,
        exported_records: records,
    }
}
fn fixture_records(fields: &TransferFields) -> Vec<TransferRecord> {
    let TransferRecord::Item(item) = &fields.record else {
        unreachable!()
    };
    vec![
        fields.record.clone(),
        TransferRecord::Observation(hsscrap::MetadataObservation {
            observation_id: "00000000-0000-0000-0000-000000000003".parse().unwrap(),
            item_id: item.item_id,
            stream_id: "00000000-0000-0000-0000-000000000005".parse().unwrap(),
            sequence: Revision(1),
            baseline_observation_id: None,
            capture_id: Some(item.first_capture_id),
            appearance_ref: None,
            producer: "rss".into(),
            access_partition: ScopeRef("origin-profile".into()),
            observed_at: item.created_at.clone(),
            source_revision: None,
            canonical_url_assertion: None,
            provider: None,
            host_site: "example.org".into(),
            title: None,
            description: None,
            author: None,
            published_at: None,
            provider_labels: vec![],
            gallery_snapshot: None,
            source_payload_digest: None,
            status: hsscrap::Availability {
                state: hsscrap::AvailabilityState::Available,
                checked_at: None,
                reason_code: None,
                retry_after: None,
            },
            common: Extensions::default(),
        }),
    ]
}

#[test]
fn rss_transfer_requires_actual_matching_observation_and_unique_exported_records() {
    let (fields, extension, origins) = fixture();
    let mut records = fixture_records(&fields);
    let keys = BTreeSet::new();
    let scope = extension.origin_scope.clone();
    let TransferRecord::Observation(observation) = &mut records[1] else {
        unreachable!()
    };
    observation.item_id = "00000000-0000-0000-0000-000000000099".parse().unwrap();
    assert!(attach_item_extension(
        fields.clone(),
        extension.clone(),
        &context(&scope, &origins, &keys, &records),
        &bounds(),
        &supported()
    )
    .is_err());
    records[1] = fixture_records(&fields).remove(1);
    records.push(records[1].clone());
    assert!(attach_item_extension(
        fields,
        extension,
        &context(&scope, &origins, &keys, &records),
        &bounds(),
        &supported()
    )
    .is_err());
}

#[test]
fn rss_transfer_roundtrip_retains_distinct_scopes_partial_coverage_and_exact_revisions() {
    let (fields, extension, origins) = fixture();
    let records = fixture_records(&fields);
    let keys = BTreeSet::new();
    let scope = extension.origin_scope.clone();
    let context = context(&scope, &origins, &keys, &records);
    let fields =
        attach_item_extension(fields, extension.clone(), &context, &bounds(), &supported())
            .unwrap();
    let plan = plan_export(fields, None, &bounds(), &supported()).unwrap();
    let bytes = serde_json::to_vec(&plan.projection).unwrap();
    let (decoded, actual) =
        decode_item_projection(&bytes, &context, &bounds(), &supported()).unwrap();
    assert_eq!(actual, Some(extension));
    assert_eq!(decoded, plan.projection);
    assert_ne!(scope, decoded.fields.audience_ref);
    assert!(String::from_utf8(bytes)
        .unwrap()
        .contains("9007199254740993"));
}

#[test]
fn rss_transfer_requires_negotiated_feature_and_existing_hsscrap_hash() {
    let (fields, extension, origins) = fixture();
    let records = fixture_records(&fields);
    let keys = BTreeSet::new();
    let scope = extension.origin_scope.clone();
    let context = context(&scope, &origins, &keys, &records);
    let fields =
        attach_item_extension(fields, extension, &context, &bounds(), &supported()).unwrap();
    let plan = plan_export(fields.clone(), None, &bounds(), &supported()).unwrap();
    let bytes = serde_json::to_vec(&plan.projection).unwrap();
    assert!(decode_item_projection(&bytes, &context, &bounds(), &BTreeSet::new()).is_err());
    let mut changed = serde_json::to_value(&plan.projection).unwrap();
    changed["fields"]["extensions"][REQUIRED_FEATURE]["states"][0]["read"] = false.into();
    assert!(decode_item_projection(
        &serde_json::to_vec(&changed).unwrap(),
        &context,
        &bounds(),
        &supported()
    )
    .is_err());
    let mut extra = fields.clone();
    extra.extensions.get_mut(REQUIRED_FEATURE).unwrap()["appearances"][0]["unknown_field"] =
        true.into();
    let extra = plan_export(extra, None, &bounds(), &supported()).unwrap();
    assert!(decode_item_projection(
        &serde_json::to_vec(&extra.projection).unwrap(),
        &context,
        &bounds(),
        &supported()
    )
    .is_err());
    let mut fields = fields;
    fields.required_features.remove(REQUIRED_FEATURE);
    let unsigned_semantics = plan_export(fields, None, &bounds(), &supported()).unwrap();
    assert!(decode_item_projection(
        &serde_json::to_vec(&unsigned_semantics.projection).unwrap(),
        &context,
        &bounds(),
        &supported()
    )
    .is_err());
}

#[test]
fn rss_transfer_refuses_missing_export_references_and_foreign_scope() {
    let (fields, extension, origins) = fixture();
    let records = fixture_records(&fields);
    let keys = BTreeSet::new();
    for reference in &origins {
        if matches!(reference, OriginRef::Capture(_)) {
            continue;
        }
        let mut missing = origins.clone();
        missing.remove(reference);
        assert!(attach_item_extension(
            fields.clone(),
            extension.clone(),
            &context(&extension.origin_scope, &missing, &keys, &records),
            &bounds(),
            &supported()
        )
        .is_err());
    }
    let foreign = ScopeRef("another-profile".into());
    assert!(attach_item_extension(
        fields,
        extension,
        &context(&foreign, &origins, &keys, &records),
        &bounds(),
        &supported()
    )
    .is_err());
}

#[test]
fn rss_transfer_rejects_cross_item_source_state_and_duplicate_identities() {
    let (fields, extension, origins) = fixture();
    let records = fixture_records(&fields);
    let keys = BTreeSet::new();
    let scope = extension.origin_scope.clone();
    let context = context(&scope, &origins, &keys, &records);
    let mut mutations = Vec::new();
    let mut changed = extension.clone();
    changed.sources.push(changed.sources[0].clone());
    mutations.push(changed);
    let mut changed = extension.clone();
    changed.sources[0].source_ref = reference("missing");
    mutations.push(changed);
    let mut changed = extension.clone();
    changed.appearances.push(changed.appearances[0].clone());
    mutations.push(changed);
    let mut changed = extension.clone();
    changed.appearances[0].access_partition = ScopeRef("foreign".into());
    mutations.push(changed);
    let mut changed = extension.clone();
    changed.appearances[0].item_id = "00000000-0000-0000-0000-000000000099".parse().unwrap();
    mutations.push(changed);
    let mut changed = extension.clone();
    changed.states.push(changed.states[0].clone());
    mutations.push(changed);
    let mut changed = extension.clone();
    changed.states[0].appearance_ref = reference("missing");
    mutations.push(changed);
    let mut changed = extension.clone();
    changed.states[0].scope = ScopeRef("foreign".into());
    mutations.push(changed);
    let mut changed = extension.clone();
    let tag = changed.states[0].legacy_tag_refs[0].clone();
    changed.states[0].legacy_tag_refs.push(tag);
    mutations.push(changed);
    let mut changed = extension.clone();
    changed.states[0].state_ref = changed.sources[0].source_ref.clone();
    mutations.push(changed);
    for changed in mutations {
        assert!(
            attach_item_extension(fields.clone(), changed, &context, &bounds(), &supported())
                .is_err()
        );
    }
}

#[test]
fn rss_transfer_metadata_is_explicitly_approved_and_bounded_before_serialization() {
    let (fields, mut extension, origins) = fixture();
    let records = fixture_records(&fields);
    let scope = extension.origin_scope.clone();
    extension.states[0].common.extensions.insert(
        "host.reviewed".into(),
        serde_json::json!({"future": ["keep"]}),
    );
    assert_eq!(
        attach_item_extension(
            fields.clone(),
            extension.clone(),
            &context(&scope, &origins, &BTreeSet::new(), &records),
            &bounds(),
            &supported()
        ),
        Err(Error::Denied)
    );
    let keys = BTreeSet::from(["host.reviewed".into()]);
    assert!(attach_item_extension(
        fields.clone(),
        extension.clone(),
        &context(&scope, &origins, &keys, &records),
        &bounds(),
        &supported()
    )
    .is_ok());
    let wide = serde_json::Value::Array(vec![
        serde_json::Value::Array(vec![
            serde_json::Value::Null;
            8
        ]);
        6
    ]);
    extension.states[0]
        .common
        .extensions
        .insert("host.reviewed".into(), wide);
    let mut small = bounds();
    small.max_operation_bytes = 50;
    assert_eq!(
        attach_item_extension(
            fields.clone(),
            extension.clone(),
            &context(&scope, &origins, &keys, &records),
            &small,
            &supported()
        ),
        Err(Error::LimitExceeded {
            field: "transfer.extensions".into()
        })
    );
    let mut nested = serde_json::Value::Null;
    for _ in 0..50 {
        nested = serde_json::json!([nested]);
    }
    extension.states[0]
        .common
        .extensions
        .insert("host.reviewed".into(), nested);
    assert!(attach_item_extension(
        fields,
        extension,
        &context(&scope, &origins, &keys, &records),
        &bounds(),
        &supported()
    )
    .is_err());
}

#[test]
fn rss_transfer_changes_advance_existing_export_revision_without_replacing_lineage() {
    let (fields, extension, origins) = fixture();
    let records = fixture_records(&fields);
    let keys = BTreeSet::new();
    let scope = extension.origin_scope.clone();
    let context = context(&scope, &origins, &keys, &records);
    let first = plan_export(
        attach_item_extension(
            fields.clone(),
            extension.clone(),
            &context,
            &bounds(),
            &supported(),
        )
        .unwrap(),
        None,
        &bounds(),
        &supported(),
    )
    .unwrap();
    let mut changed = extension;
    changed.states[0].read = false;
    let second = plan_export(
        attach_item_extension(fields, changed, &context, &bounds(), &supported()).unwrap(),
        Some(&first.cursor),
        &bounds(),
        &supported(),
    )
    .unwrap();
    assert_eq!(second.cursor.revision, Revision(2));
    assert_ne!(
        first.projection.payload_hash,
        second.projection.payload_hash
    );
    assert_eq!(
        first.cursor.export_stream_id,
        second.cursor.export_stream_id
    );
}

#[test]
fn rss_transfer_preserves_unextended_hsscrap_and_rejects_raw_duplicate_keys() {
    let (fields, _, origins) = fixture();
    let records = fixture_records(&fields);
    let keys = BTreeSet::new();
    let scope = ScopeRef("origin-profile".into());
    let context = context(&scope, &origins, &keys, &records);
    let plan = plan_export(fields, None, &bounds(), &BTreeSet::new()).unwrap();
    let bytes = serde_json::to_vec(&plan.projection).unwrap();
    assert_eq!(
        decode_item_projection(&bytes, &context, &bounds(), &BTreeSet::new()).unwrap(),
        (plan.projection, None)
    );
    let duplicate = String::from_utf8(bytes).unwrap().replace(
        "\"payload_hash\":",
        "\"payload_hash\":\"duplicate\",\"payload_hash\":",
    );
    assert!(
        decode_item_projection(duplicate.as_bytes(), &context, &bounds(), &BTreeSet::new())
            .is_err()
    );
}

#[test]
fn rss_transfer_rejects_overwrite_missing_required_extension_and_byte_overflow() {
    let (fields, extension, origins) = fixture();
    let records = fixture_records(&fields);
    let keys = BTreeSet::new();
    let scope = extension.origin_scope.clone();
    let context = context(&scope, &origins, &keys, &records);
    let attached = attach_item_extension(
        fields.clone(),
        extension.clone(),
        &context,
        &bounds(),
        &supported(),
    )
    .unwrap();
    assert!(attach_item_extension(
        attached,
        extension.clone(),
        &context,
        &bounds(),
        &supported()
    )
    .is_err());
    let mut missing = fields.clone();
    missing.required_features.insert(REQUIRED_FEATURE.into());
    let plan = plan_export(missing, None, &bounds(), &supported()).unwrap();
    assert!(decode_item_projection(
        &serde_json::to_vec(&plan.projection).unwrap(),
        &context,
        &bounds(),
        &supported()
    )
    .is_err());
    let mut tiny = bounds();
    tiny.max_operation_bytes = 100;
    assert!(attach_item_extension(fields, extension, &context, &tiny, &supported()).is_err());
}
