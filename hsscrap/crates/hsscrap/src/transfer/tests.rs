use super::*;
use crate::*;
use std::collections::{BTreeMap, BTreeSet};
fn uuid(n: u128) -> uuid::Uuid {
    uuid::Uuid::from_u128(n)
}
fn at() -> Instant {
    Instant::parse("2026-10-23T00:00:00Z").unwrap()
}
fn scope() -> ScopeRef {
    ScopeRef("source-scope".into())
}
fn host(kind: &str) -> HostRef {
    HostRef {
        host_instance_id: "source-host".into(),
        owner_kind: kind.into(),
        native_id: kind.into(),
    }
}
fn bounds() -> Bounds {
    Bounds {
        max_url_bytes: 4096,
        max_text_bytes: 4096,
        max_collection_items: 1000,
        max_page_items: 100,
        max_operation_bytes: 100000,
    }
}
fn available() -> Availability {
    Availability {
        state: AvailabilityState::Available,
        checked_at: None,
        reason_code: None,
        retry_after: None,
    }
}
fn common() -> Extensions {
    Extensions {
        extensions: BTreeMap::from([("vendor.keep".into(), serde_json::json!({"value":"opaque"}))]),
        required_features: BTreeSet::new(),
    }
}
fn records() -> Vec<TransferRecord> {
    let item = ItemId(uuid(1));
    let capture = CaptureId(uuid(2));
    let observation = ObservationId(uuid(3));
    let stream = StreamId(uuid(4));
    let member = MemberId(uuid(5));
    let second = MemberId(uuid(6));
    let membership = MembershipId(uuid(7));
    let snapshot = GallerySnapshot {
        coverage: Coverage::Complete,
        full_set_digest: Some(crate::gallery::gallery_digest(&[member, second]).unwrap()),
        members: [member, second]
            .into_iter()
            .map(|member_id| MemberObservation {
                member_id,
                producer_member_key: Some(
                    if member_id == member {
                        "producer-a"
                    } else {
                        "producer-b"
                    }
                    .into(),
                ),
                media_kind: MediaKind::Image,
                caption: Some("private caption".into()),
                observed_mime: Some("image/png".into()),
                observed_width: Some(10),
                observed_height: Some(10),
                observed_duration_ms: None,
                availability: available(),
                remote_reference_ids: vec![host("remote")],
                common: common(),
            })
            .collect(),
    };
    vec![
        TransferRecord::Item(Item {
            item_id: item,
            owner_scope: scope(),
            revision: Revision(1),
            first_capture_id: capture,
            selected_observation_id: Some(observation),
            gallery_observation_id: Some(observation),
            member_order: vec![member, second],
            title_override: Override::Inherit,
            description_override: Override::Inherit,
            cover_override: Override::Set(second),
            created_at: at(),
            updated_at: at(),
            lifecycle: Lifecycle::Active,
            deleted_at: None,
            common: common(),
        }),
        TransferRecord::Capture(URLCapture {
            capture_id: capture,
            item_id: item,
            capture_kind: CaptureKind::LegacyMigration,
            access_partition: scope(),
            captured_url: "https://example.org/first".into(),
            original_url: None,
            identity_url: "https://example.org/first".into(),
            normalization_version: "hsscrap.url.v1".into(),
            captured_at: None,
            created_here_at: at(),
            actor_ref: Some(host("actor")),
            operation_id: OperationId(uuid(8)),
            common: common(),
        }),
        TransferRecord::Identity(SourceIdentity {
            identity_id: IdentityId(uuid(9)),
            item_id: item,
            provider: "provider".into(),
            namespace: "namespace".into(),
            provider_item_id: "opaque".into(),
            access_partition: scope(),
            verified_by_observation_id: observation,
            retired_at: None,
            common: common(),
        }),
        TransferRecord::Observation(MetadataObservation {
            observation_id: observation,
            item_id: item,
            stream_id: stream,
            sequence: Revision(1),
            baseline_observation_id: None,
            capture_id: Some(capture),
            appearance_ref: Some(host("appearance")),
            producer: "fixture".into(),
            access_partition: scope(),
            observed_at: at(),
            source_revision: None,
            canonical_url_assertion: None,
            provider: None,
            host_site: "example.org".into(),
            title: Some("title".into()),
            description: None,
            author: None,
            published_at: None,
            provider_labels: vec![],
            gallery_snapshot: Some(snapshot),
            source_payload_digest: None,
            status: available(),
            common: common(),
        }),
        TransferRecord::Stream(ObservationStream {
            stream_id: stream,
            item_id: item,
            producer: "fixture".into(),
            provider_namespace: "namespace".into(),
            source_ref: host("source"),
            access_partition: scope(),
            last_issued_sequence: Revision(1),
            last_accepted_sequence: Some(Revision(1)),
            latest_observation_id: Some(observation),
            revision: Revision(1),
            common: common(),
        }),
        TransferRecord::Member(GalleryMember {
            member_id: member,
            item_id: item,
            producer_member_key: Some("member".into()),
            observation_id: observation,
            media_kind: MediaKind::Image,
            caption: Some("caption".into()),
            observed_mime: Some("image/png".into()),
            observed_width: Some(10),
            observed_height: Some(10),
            observed_duration_ms: None,
            availability: available(),
            lifecycle: MemberLifecycle::Active,
            removed_at: None,
            common: common(),
        }),
        TransferRecord::Remote(RemoteMediaReference {
            reference_id: host("remote"),
            member_ref: MemberRef {
                item_id: item,
                member_id: member,
            },
            observation_id: observation,
            role: RemoteRole::Original,
            target: RemoteTarget::PrivateLocator(host("locator")),
            expires_at: None,
            access_partition: scope(),
            availability: available(),
            common: common(),
        }),
        TransferRecord::Binding(RetainedMediaBinding {
            binding_id: BindingId(uuid(10)),
            member_ref: MemberRef {
                item_id: item,
                member_id: member,
            },
            media_ref: MediaRef(host("media")),
            media_revision_ref: "source-media-revision".into(),
            role: RetainedRole::Original,
            materialization_receipt_ref: host("foreign-receipt"),
            created_at: at(),
            derived_from_binding_id: None,
            common: common(),
        }),
        TransferRecord::Assignment(TagAssignment {
            assignment_id: AssignmentId(uuid(11)),
            tag_ref: TagRef(host("tag")),
            target: TagTarget::Item(item),
            scope: scope(),
            origin: TagOrigin::User,
            created_at: at(),
            removed_at: None,
            revision: Revision(1),
            common: common(),
        }),
        TransferRecord::Membership(ProjectMembership {
            membership_id: membership,
            item_id: item,
            project_ref: ProjectRef(host("project")),
            scope: scope(),
            saved_at: at(),
            created_here_at: at(),
            revision: Revision(1),
            lifecycle: Lifecycle::Active,
            removed_at: None,
            common: common(),
        }),
        TransferRecord::Placement(FolderPlacement {
            placement_id: PlacementId(uuid(12)),
            item_id: item,
            membership_id: Some(membership),
            folder_ref: Some(FolderRef(host("folder"))),
            scope: scope(),
            sort_order: Some(1),
            placed_at: at(),
            moved_at: None,
            created_here_at: at(),
            revision: Revision(1),
            lifecycle: Lifecycle::Active,
            removed_at: None,
            common: common(),
        }),
        TransferRecord::Occurrence(OccurrenceReference {
            occurrence_ref: OccurrenceRef(host("occurrence")),
            item_id: item,
            member_id: Some(member),
            context_membership_id: Some(membership),
            caption_override: Override::Inherit,
            presentation: None,
            revision: Revision(1),
            common: common(),
        }),
    ]
}
fn fields(record: TransferRecord) -> TransferFields {
    let (origin_kind, origin_id) = super::model::origin_parts(&record.origin()).unwrap();
    TransferFields {
        model_version: MODEL_VERSION.into(),
        export_stream_id: ExportStreamId(uuid(20)),
        origin_instance_id: "source-instance".into(),
        origin_kind,
        origin_id,
        revision: Revision(1),
        projection_policy_id: "authorized.v1".into(),
        audience_ref: ScopeRef("destination-audience".into()),
        record,
        required_features: BTreeSet::new(),
        extensions: BTreeMap::new(),
    }
}
fn export(record: TransferRecord) -> ExportPlan {
    plan_export(fields(record), None, &bounds(), &BTreeSet::new()).unwrap()
}
fn envelope(source: &TransferProjection) -> VerifiedEnvelope {
    let f = &source.fields;
    VerifiedEnvelope {
        origin_instance_id: f.origin_instance_id.clone(),
        export_stream_id: f.export_stream_id,
        audience_ref: f.audience_ref.clone(),
        projection_policy_id: f.projection_policy_id.clone(),
    }
}
fn moved_host(mut value: HostRef) -> HostRef {
    value.host_instance_id = "destination-host".into();
    value.native_id.push_str("-local");
    value
}
fn moved(r: OriginRef) -> OriginRef {
    macro_rules! id {
        ($v:expr,$kind:ident,$id:ident) => {
            OriginRef::$kind($id(uuid($v.0.as_u128() + 100)))
        };
    }
    match r {
        OriginRef::Item(v) => id!(v, Item, ItemId),
        OriginRef::Capture(v) => id!(v, Capture, CaptureId),
        OriginRef::Identity(v) => id!(v, Identity, IdentityId),
        OriginRef::Observation(v) => id!(v, Observation, ObservationId),
        OriginRef::Stream(v) => id!(v, Stream, StreamId),
        OriginRef::Member(v) => id!(v, Member, MemberId),
        OriginRef::Binding(v) => id!(v, Binding, BindingId),
        OriginRef::Assignment(v) => id!(v, Assignment, AssignmentId),
        OriginRef::Membership(v) => id!(v, Membership, MembershipId),
        OriginRef::Placement(v) => id!(v, Placement, PlacementId),
        OriginRef::Operation(v) => id!(v, Operation, OperationId),
        OriginRef::Tag(v) => OriginRef::Tag(TagRef(moved_host(v.0))),
        OriginRef::Project(v) => OriginRef::Project(ProjectRef(moved_host(v.0))),
        OriginRef::Folder(v) => OriginRef::Folder(FolderRef(moved_host(v.0))),
        OriginRef::Media(v) => OriginRef::Media(MediaRef(moved_host(v.0))),
        OriginRef::Occurrence(v) => OriginRef::Occurrence(OccurrenceRef(moved_host(v.0))),
        OriginRef::Remote(v) => OriginRef::Remote(moved_host(v)),
        OriginRef::Host(v) => OriginRef::Host(moved_host(v)),
        OriginRef::Scope(_) => OriginRef::Scope(ScopeRef("destination-scope".into())),
    }
}
fn mappings(records: &[TransferRecord]) -> DestinationMappings {
    let mut mappings = DestinationMappings {
        origin_instance_id: "source-instance".into(),
        destination_scope: ScopeRef("destination-scope".into()),
        references: BTreeMap::new(),
        local_record_refs: BTreeMap::new(),
        materialized_bindings: BTreeMap::new(),
    };
    for record in records {
        for r in super::references::references(record).unwrap() {
            mappings.references.insert(r.clone(), moved(r));
        }
        mappings.local_record_refs.insert(
            record.origin(),
            HostRef {
                host_instance_id: "destination-host".into(),
                owner_kind: "canonical-owner".into(),
                native_id: format!("local-{:?}", record.origin()),
            },
        );
        if let TransferRecord::Binding(v) = record {
            let mut local = record.clone();
            super::references::map_references(&mut local, &mut |r| Ok(moved(r))).unwrap();
            let TransferRecord::Binding(mut local) = local else {
                unreachable!()
            };
            local.materialization_receipt_ref = HostRef {
                host_instance_id: "destination-host".into(),
                owner_kind: "media.materialization".into(),
                native_id: "verified-destination-bytes".into(),
            };
            local.media_revision_ref = "destination-media-revision".into();
            mappings.materialized_bindings.insert(v.binding_id, local);
        }
    }
    mappings
}
fn import(
    source: &TransferProjection,
    mappings: &DestinationMappings,
    previous: Option<&ImportMapping>,
    current: Option<&TransferRecord>,
    revision: Revision,
    resolution: ImportResolution,
) -> ImportPlan {
    plan_import(
        source,
        &envelope(source),
        mappings,
        previous,
        current,
        revision,
        at(),
        resolution,
        &bounds(),
        &BTreeSet::new(),
    )
    .unwrap()
}

#[test]
fn typed_reference_kinds_roundtrip_with_explicit_destination_owners_and_local_media_receipt() {
    let mut records = records();
    for (index, target) in [
        TagTarget::Project(ProjectRef(host("project"))),
        TagTarget::Membership(MembershipId(uuid(7))),
        TagTarget::Folder(FolderRef(host("folder"))),
        TagTarget::Occurrence(OccurrenceRef(host("occurrence"))),
    ]
    .into_iter()
    .enumerate()
    {
        let TransferRecord::Assignment(mut assignment) = records[8].clone() else {
            panic!()
        };
        assignment.assignment_id = AssignmentId(uuid(30 + index as u128));
        assignment.target = target;
        records.push(TransferRecord::Assignment(assignment));
    }
    let mappings = mappings(&records);
    for record in records {
        let source = export(record.clone()).projection;
        let wire = serde_json::to_vec(&source).unwrap();
        assert_eq!(
            decode_projection(&wire, &bounds(), &BTreeSet::new()).unwrap(),
            source
        );
        let ImportPlan::Apply {
            record: local,
            mapping,
            source_payload_hash,
            source_gallery_digest,
        } = import(
            &source,
            &mappings,
            None,
            None,
            Revision(1),
            ImportResolution::Preserve,
        )
        else {
            panic!("not applied: {:?}", record.origin())
        };
        assert_eq!(mapping.local_revision_at_import, Revision(2));
        assert_eq!(source_payload_hash, source.payload_hash);
        let text = serde_json::to_string(&local).unwrap();
        for n in 1..=12 {
            assert!(
                !text.contains(&format!("\"{}\"", uuid(n))),
                "foreign UUID retained: {text}"
            );
        }
        assert!(!text.contains("\"host_instance_id\":\"source-host\""));
        assert!(text.contains("vendor.keep"));
        if let TransferRecord::Observation(v) = &local {
            let g = v.gallery_snapshot.as_ref().unwrap();
            assert_eq!(
                g.full_set_digest,
                Some(
                    crate::gallery::gallery_digest(&[MemberId(uuid(105)), MemberId(uuid(106))])
                        .unwrap()
                )
            );
            assert_ne!(g.full_set_digest, source_gallery_digest);
        }
        if let TransferRecord::Binding(v) = &local {
            assert_eq!(
                v.materialization_receipt_ref.native_id,
                "verified-destination-bytes"
            );
            assert_eq!(v.media_revision_ref, "destination-media-revision");
        }
    }
}

#[test]
fn export_cursor_reuses_content_and_fences_revision_lineage_and_ordered_arrays() {
    let first = export(records()[0].clone());
    let same = plan_export(
        first.projection.fields.clone(),
        Some(&first.cursor),
        &bounds(),
        &BTreeSet::new(),
    )
    .unwrap();
    assert_eq!(same, first);
    let mut changed = first.projection.fields.clone();
    if let TransferRecord::Item(v) = &mut changed.record {
        v.member_order.reverse();
    }
    let next = plan_export(changed, Some(&first.cursor), &bounds(), &BTreeSet::new()).unwrap();
    assert_eq!(next.cursor.revision, Revision(2));
    assert_ne!(next.projection.payload_hash, first.projection.payload_hash);
    let mut audience = first.projection.fields.clone();
    audience.audience_ref = ScopeRef("another-audience".into());
    assert!(plan_export(audience, Some(&first.cursor), &bounds(), &BTreeSet::new()).is_err());
    let mut overflow = first.cursor;
    overflow.revision = Revision(u64::MAX);
    overflow.content_hash = "changed".into();
    assert!(plan_export(
        first.projection.fields,
        Some(&overflow),
        &bounds(),
        &BTreeSet::new()
    )
    .is_err());
}

#[test]
fn import_replays_after_restart_and_fences_old_equal_hash_conflicts_streams_and_audience() {
    let records = records();
    let maps = mappings(&records);
    let first = export(records[0].clone());
    let ImportPlan::Apply {
        record: local,
        mapping,
        ..
    } = import(
        &first.projection,
        &maps,
        None,
        None,
        Revision(1),
        ImportResolution::Preserve,
    )
    else {
        panic!()
    };
    let mapping: ImportMapping =
        serde_json::from_slice(&serde_json::to_vec(&mapping).unwrap()).unwrap();
    assert!(matches!(
        import(
            &first.projection,
            &maps,
            Some(&mapping),
            Some(&local),
            Revision(2),
            ImportResolution::Preserve
        ),
        ImportPlan::Reuse { .. }
    ));
    let mut changed = first.projection.fields.clone();
    changed
        .extensions
        .insert("vendor.change".into(), true.into());
    let collision = plan_export(changed.clone(), None, &bounds(), &BTreeSet::new()).unwrap();
    assert!(
        matches!(import(&collision.projection,&maps,Some(&mapping),Some(&local),Revision(2),ImportResolution::Preserve),ImportPlan::Conflict{field} if field=="equal_revision_hash")
    );
    let next = plan_export(changed, Some(&first.cursor), &bounds(), &BTreeSet::new()).unwrap();
    let ImportPlan::Apply {
        record: local,
        mapping,
        ..
    } = import(
        &next.projection,
        &maps,
        Some(&mapping),
        Some(&local),
        Revision(2),
        ImportResolution::Resolve {
            expected_local_revision: Revision(2),
        },
    )
    else {
        panic!()
    };
    assert!(
        matches!(import(&first.projection,&maps,Some(&mapping),Some(&local),Revision(3),ImportResolution::Preserve),ImportPlan::Conflict{field} if field=="older_revision")
    );
    let mut other = next.projection.fields;
    other.export_stream_id = ExportStreamId(uuid(21));
    let other = plan_export(other, None, &bounds(), &BTreeSet::new()).unwrap();
    assert!(
        matches!(import(&other.projection,&maps,Some(&mapping),Some(&local),Revision(3),ImportResolution::Preserve),ImportPlan::Conflict{field} if field=="different_export_stream")
    );
    let mut wrong = envelope(&first.projection);
    wrong.audience_ref = ScopeRef("other".into());
    assert_eq!(
        plan_import(
            &first.projection,
            &wrong,
            &maps,
            None,
            None,
            Revision(1),
            at(),
            ImportResolution::Preserve,
            &bounds(),
            &BTreeSet::new()
        ),
        Err(Error::Denied)
    );
}

#[test]
fn missing_typed_mappings_and_foreign_media_receipts_quarantine_without_uuid_fallback() {
    let records = records();
    let mut maps = mappings(&records);
    let source = export(records[1].clone());
    maps.references
        .remove(&OriginRef::Operation(OperationId(uuid(8))));
    assert!(
        matches!(import(&source.projection,&maps,None,None,Revision(1),ImportResolution::Preserve),ImportPlan::Quarantined{reason:QuarantineReason::MissingReferences(v)} if v.contains(&OriginRef::Operation(OperationId(uuid(8)))))
    );
    let source = export(records[7].clone());
    maps.materialized_bindings.clear();
    assert!(matches!(
        import(
            &source.projection,
            &maps,
            None,
            None,
            Revision(1),
            ImportResolution::Preserve
        ),
        ImportPlan::Quarantined {
            reason: QuarantineReason::MediaMaterialization(_)
        }
    ));
}

#[test]
fn filtered_capture_and_gallery_downgrade_without_exposing_denied_ids_or_mutating_source() {
    let mut records = records();
    let before = records.clone();
    let TransferRecord::Capture(mut second) = records[1].clone() else {
        panic!()
    };
    second.capture_id = CaptureId(uuid(30));
    second.captured_url = "https://example.org/authorized".into();
    second.identity_url = second.captured_url.clone();
    records.push(TransferRecord::Capture(second));
    let TransferRecord::Member(mut second_member) = records[5].clone() else {
        panic!()
    };
    second_member.member_id = MemberId(uuid(6));
    records.push(TransferRecord::Member(second_member));
    let mut authorized = BTreeSet::new();
    for r in &records {
        authorized.extend(super::references::references(r).unwrap());
    }
    authorized.remove(&OriginRef::Capture(CaptureId(uuid(2))));
    authorized.remove(&OriginRef::Member(MemberId(uuid(6))));
    let policy = ProjectionPolicy {
        policy_id: "policy".into(),
        audience_ref: ScopeRef("audience".into()),
        authorized,
        readable_members: BTreeSet::new(),
        capture_order: vec![CaptureId(uuid(2)), CaptureId(uuid(30))],
    };
    let projected = project_records(&records, &policy, &bounds(), &BTreeSet::new()).unwrap();
    let item = projected
        .iter()
        .find_map(|r| {
            if let TransferRecord::Item(v) = r {
                Some(v)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(item.first_capture_id, CaptureId(uuid(30)));
    assert_eq!(item.member_order, vec![MemberId(uuid(5))]);
    assert_eq!(item.cover_override, Override::Inherit);
    assert_eq!(
        item.common.extensions["hsscrap.capture_history_partial"],
        true
    );
    let observation = projected
        .iter()
        .find_map(|r| {
            if let TransferRecord::Observation(v) = r {
                Some(v)
            } else {
                None
            }
        })
        .unwrap();
    let g = observation.gallery_snapshot.as_ref().unwrap();
    assert_eq!(g.coverage, Coverage::Partial);
    assert_eq!(g.full_set_digest, None);
    assert_eq!(g.members.len(), 1);
    assert_eq!(g.members[0].caption, None);
    assert!(g.members[0].remote_reference_ids.is_empty());
    let text = serde_json::to_string(&projected).unwrap();
    assert!(!text.contains(&uuid(2).to_string()));
    assert!(!text.contains(&uuid(6).to_string()));
    assert!(!text.contains("private caption"));
    assert!(!text.contains("https://example.org/first"));
    let mut locator_denied = policy.clone();
    locator_denied.readable_members.insert(MemberId(uuid(5)));
    locator_denied
        .authorized
        .remove(&OriginRef::Host(host("locator")));
    let denied = project_records(&records, &locator_denied, &bounds(), &BTreeSet::new()).unwrap();
    for record in &denied {
        match record {
            TransferRecord::Member(v) => {
                assert_eq!(v.caption, None);
                assert_eq!(v.availability.state, AvailabilityState::Restricted);
                assert!(v.common.extensions.is_empty());
            }
            TransferRecord::Observation(v) => {
                let member = &v.gallery_snapshot.as_ref().unwrap().members[0];
                assert_eq!(member.caption, None);
                assert!(member.common.extensions.is_empty());
            }
            TransferRecord::Remote(v) => assert!(matches!(v.target, RemoteTarget::Unavailable(_))),
            _ => {}
        }
    }
    // Expiry alone is not a permission denial.
    let mut expired = records.clone();
    if let TransferRecord::Remote(v) = &mut expired[6] {
        v.expires_at = Some(at());
    }
    let mut readable = policy.clone();
    readable.readable_members.insert(MemberId(uuid(5)));
    let expired = project_records(&expired, &readable, &bounds(), &BTreeSet::new()).unwrap();
    let caption = expired.iter().find_map(|r| {
        if let TransferRecord::Member(v) = r {
            v.caption.as_deref()
        } else {
            None
        }
    });
    assert_eq!(caption, Some("caption"));
    assert_eq!(&records[..before.len()], before.as_slice());
}

#[test]
fn local_overrides_tombstones_and_removed_tags_survive_import_until_explicit_resolution() {
    let records = records();
    let maps = mappings(&records);
    let first = export(records[0].clone());
    let ImportPlan::Apply {
        mut record,
        mapping,
        ..
    } = import(
        &first.projection,
        &maps,
        None,
        None,
        Revision(1),
        ImportResolution::Preserve,
    )
    else {
        panic!()
    };
    if let TransferRecord::Item(v) = &mut record {
        v.revision = Revision(3);
        v.title_override = Override::Set("local work".into());
        v.lifecycle = Lifecycle::Tombstoned;
        v.deleted_at = Some(at());
    }
    let mut changed = first.projection.fields.clone();
    changed.extensions.insert("vendor.new".into(), true.into());
    let next = plan_export(changed, Some(&first.cursor), &bounds(), &BTreeSet::new()).unwrap();
    assert!(matches!(
        import(
            &next.projection,
            &maps,
            Some(&mapping),
            Some(&record),
            Revision(3),
            ImportResolution::Preserve
        ),
        ImportPlan::Conflict { .. }
    ));
    let mut deleted = next.projection.fields.clone();
    if let TransferRecord::Item(v) = &mut deleted.record {
        v.lifecycle = Lifecycle::Tombstoned;
        v.deleted_at = Some(at());
    }
    let deleted = plan_export(deleted, Some(&next.cursor), &bounds(), &BTreeSet::new()).unwrap();
    assert!(
        matches!(import(&deleted.projection,&maps,Some(&mapping),Some(&record),Revision(3),ImportResolution::Preserve),ImportPlan::ProvenanceOnly{mapping} if mapping.state==ImportState::SourceDeleted)
    );
    assert_eq!(
        plan_revocation(&mapping, &bounds(), &BTreeSet::new())
            .unwrap()
            .state,
        ImportState::Revoked
    );
    let first = export(records[8].clone());
    let ImportPlan::Apply {
        mut record,
        mapping,
        ..
    } = import(
        &first.projection,
        &maps,
        None,
        None,
        Revision(1),
        ImportResolution::Preserve,
    )
    else {
        panic!()
    };
    if let TransferRecord::Assignment(v) = &mut record {
        v.removed_at = Some(at());
        v.revision = Revision(3);
    }
    let mut changed = first.projection.fields.clone();
    changed.extensions.insert("vendor.new".into(), true.into());
    let next = plan_export(changed, Some(&first.cursor), &bounds(), &BTreeSet::new()).unwrap();
    assert!(matches!(
        import(
            &next.projection,
            &maps,
            Some(&mapping),
            Some(&record),
            Revision(3),
            ImportResolution::Preserve
        ),
        ImportPlan::Conflict { .. }
    ));
}

#[test]
fn complete_fields_hash_nulls_extensions_numeric_bounds_and_raw_duplicates_are_enforced() {
    let mut f = fields(records()[1].clone());
    let features = BTreeSet::from(["vendor.\r".into(), "vendor.1".into()]);
    f.required_features = features.clone();
    let p = plan_export(f, None, &bounds(), &features)
        .unwrap()
        .projection;
    let bytes = canonical_fields_bytes(&p.fields).unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(text.contains("\"original_url\":null"));
    assert!(text.contains("\"required_features\":[\"vendor.1\",\"vendor.\\r\"]"));
    assert_eq!(p.payload_hash, handshake_canonical_json::sha256_hex(&bytes));
    let mut corrupt = p.clone();
    corrupt
        .fields
        .extensions
        .insert("vendor.tampered".into(), true.into());
    assert!(matches!(plan_import(
        &corrupt,
        &envelope(&corrupt),
        &mappings(&records()),
        None,
        None,
        Revision(1),
        at(),
        ImportResolution::Preserve,
        &bounds(),
        &features
    ),Err(Error::Conflict{field}) if field=="payload_hash"));
    let mut f = fields(records()[1].clone());
    f.extensions
        .insert("vendor.big".into(), serde_json::json!(u64::MAX));
    assert!(plan_export(f, None, &bounds(), &BTreeSet::new()).is_err());
    let input = br#"{"fields":null,"fields":null,"payload_hash":"x"}"#;
    assert!(decode_projection(input, &bounds(), &BTreeSet::new()).is_err());
    let mut tiny = bounds();
    tiny.max_operation_bytes = 10;
    assert!(decode_projection(&serde_json::to_vec(&p).unwrap(), &tiny, &features).is_err());
    let mut f = fields(records()[1].clone());
    let mut deep = serde_json::Value::Null;
    for _ in 0..70 {
        deep = serde_json::json!([deep]);
    }
    f.extensions.insert("vendor.deep".into(), deep);
    assert!(plan_export(f, None, &bounds(), &BTreeSet::new()).is_err());
    let mut record = records()[3].clone();
    if let TransferRecord::Observation(v) = &mut record {
        v.gallery_snapshot.as_mut().unwrap().full_set_digest = Some("bad-source-digest".into());
    }
    let p = export(record).projection;
    assert!(
        matches!(import(&p,&mappings(&records()),None,None,Revision(1),ImportResolution::Preserve),ImportPlan::Conflict{field} if field=="source_gallery_digest")
    );
}

#[test]
fn typed_record_depth_is_rejected_before_projection_and_current_import_cloning() {
    let mut deep = serde_json::Value::Null;
    for _ in 0..65 {
        deep = serde_json::json!([deep]);
    }
    let mut source = records();
    let TransferRecord::Capture(v) = &mut source[1] else {
        panic!()
    };
    v.common
        .extensions
        .insert("vendor.deep".into(), deep.clone());
    let before = source.clone();
    let policy = ProjectionPolicy {
        policy_id: "policy".into(),
        audience_ref: scope(),
        authorized: BTreeSet::new(),
        readable_members: BTreeSet::new(),
        capture_order: vec![CaptureId(uuid(2))],
    };
    assert!(
        matches!(project_records(&source,&policy,&bounds(),&BTreeSet::new()),Err(Error::Invalid{field,..}) if field=="transfer")
    );
    assert_eq!(source, before);
    let records = records();
    let maps = mappings(&records);
    let projection = export(records[0].clone()).projection;
    let ImportPlan::Apply {
        mut record,
        mapping,
        ..
    } = import(
        &projection,
        &maps,
        None,
        None,
        Revision(1),
        ImportResolution::Preserve,
    )
    else {
        panic!()
    };
    let TransferRecord::Item(v) = &mut record else {
        panic!()
    };
    v.common.extensions.insert("vendor.deep".into(), deep);
    let before = record.clone();
    assert!(
        matches!(plan_import(&projection,&envelope(&projection),&maps,Some(&mapping),Some(&record),Revision(2),at(),ImportResolution::Preserve,&bounds(),&BTreeSet::new()),Err(Error::Invalid{field,..}) if field=="transfer")
    );
    assert_eq!(record, before);
}

#[test]
fn raw_field_shape_is_verified_without_dropping_unknown_properties_or_opaque_extensions() {
    let p = export(records()[1].clone()).projection;
    let original = serde_json::to_value(&p).unwrap();
    let wire = serde_json::to_vec(&original).unwrap();
    assert_eq!(
        decode_projection(&wire, &bounds(), &BTreeSet::new()).unwrap(),
        p
    );
    for (location, key) in [
        ("wrapper", "extra"),
        ("fields", "extra"),
        ("record", "extra"),
        ("record", "original_url"),
        ("record", "extensions"),
    ] {
        let mut raw = original.clone();
        let target = match location {
            "wrapper" => &mut raw,
            "fields" => &mut raw["fields"],
            _ => &mut raw["fields"]["record"]["value"],
        };
        if key == "extra" {
            target[key] = true.into();
        } else {
            target.as_object_mut().unwrap().remove(key);
        }
        assert!(
            decode_projection(
                &serde_json::to_vec(&raw).unwrap(),
                &bounds(),
                &BTreeSet::new()
            )
            .is_err(),
            "accepted {location}:{key}"
        );
    }
    let mut fields = p.fields;
    fields.extensions.insert(
        "vendor.opaque".into(),
        serde_json::json!({"required_features":["z","a"],"unknown":{"null":null,"order":[3,1,2]}}),
    );
    let p = plan_export(fields, None, &bounds(), &BTreeSet::new())
        .unwrap()
        .projection;
    let decoded = decode_projection(
        &serde_json::to_vec(&p).unwrap(),
        &bounds(),
        &BTreeSet::new(),
    )
    .unwrap();
    assert_eq!(decoded, p);
    assert_eq!(
        decoded.fields.extensions["vendor.opaque"]["required_features"],
        serde_json::json!(["z", "a"])
    );
}
