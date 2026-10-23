use hsscrap::interaction::{Inspection, TagChoice, TargetAccess};
use hsscrap::ports::CommandContext;
use hsscrap::*;

pub fn host(kind: &str, id: &str) -> HostRef {
    HostRef {
        host_instance_id: "gui-proof".into(),
        owner_kind: kind.into(),
        native_id: id.into(),
    }
}
pub fn at() -> Instant {
    Instant::parse("2026-10-23T08:00:00Z").unwrap()
}
pub fn tag(id: &str) -> TagRef {
    TagRef(host("tag", id))
}
pub fn item_id() -> ItemId {
    ItemId(uuid::Uuid::from_u128(1))
}
pub fn project() -> TagTarget {
    TagTarget::Project(ProjectRef(host("project", "reading")))
}
pub fn bounds() -> Bounds {
    Bounds {
        max_url_bytes: 4096,
        max_text_bytes: 4096,
        max_collection_items: 100,
        max_page_items: 25,
        max_operation_bytes: 32768,
    }
}
pub fn assignment(id: u128, name: &str, target: TagTarget) -> TagAssignment {
    TagAssignment {
        assignment_id: AssignmentId(uuid::Uuid::from_u128(id)),
        tag_ref: tag(name),
        target,
        scope: ScopeRef("private".into()),
        origin: TagOrigin::User,
        created_at: at(),
        removed_at: None,
        revision: Revision(1),
        common: Extensions::default(),
    }
}
pub fn inspection() -> Inspection {
    let observation = MetadataObservation {
        observation_id: ObservationId(uuid::Uuid::from_u128(10)),
        item_id: item_id(),
        stream_id: StreamId(uuid::Uuid::from_u128(11)),
        sequence: Revision(1),
        baseline_observation_id: None,
        capture_id: Some(CaptureId(uuid::Uuid::from_u128(12))),
        appearance_ref: None,
        producer: "Example publisher".into(),
        access_partition: ScopeRef("private".into()),
        observed_at: at(),
        source_revision: None,
        canonical_url_assertion: Some("https://example.org/design-notes".into()),
        provider: Some("example".into()),
        host_site: "example.org".into(),
        title: Some("Source title: a field guide to collections".into()),
        description: Some("Publisher description retained in the immutable observation.".into()),
        author: None,
        published_at: None,
        provider_labels: vec![],
        gallery_snapshot: None,
        source_payload_digest: None,
        status: Availability {
            state: AvailabilityState::Available,
            checked_at: Some(at()),
            reason_code: None,
            retry_after: None,
        },
        common: Extensions::default(),
    };
    let item = Item {
        item_id: item_id(),
        owner_scope: ScopeRef("private".into()),
        revision: Revision(2),
        first_capture_id: CaptureId(uuid::Uuid::from_u128(12)),
        selected_observation_id: Some(observation.observation_id),
        gallery_observation_id: None,
        member_order: vec![],
        title_override: Override::Set("A field guide worth keeping".into()),
        description_override: Override::Clear,
        cover_override: Override::Inherit,
        created_at: at(),
        updated_at: at(),
        lifecycle: Lifecycle::Active,
        deleted_at: None,
        common: Extensions::default(),
    };
    let folder = TagTarget::Folder(FolderRef(host("folder", "reference")));
    let mut legacy = assignment(23, "reference", folder.clone());
    legacy.origin = TagOrigin::LegacyUnknown;
    Inspection {
        scope: ScopeRef("private".into()),
        item,
        selected_observation: Some(observation),
        captures: vec![URLCapture {
            capture_id: CaptureId(uuid::Uuid::from_u128(12)),
            item_id: item_id(),
            capture_kind: CaptureKind::LegacyMigration,
            access_partition: ScopeRef("private".into()),
            captured_url: "https://example.org/design-notes?from=reader".into(),
            original_url: None,
            identity_url: "https://example.org/design-notes?from=reader".into(),
            normalization_version: hsscrap::identity::NORMALIZATION_VERSION.into(),
            captured_at: None,
            created_here_at: at(),
            actor_ref: None,
            operation_id: OperationId(uuid::Uuid::from_u128(13)),
            common: Extensions::default(),
        }],
        memberships: vec![ProjectMembership {
            membership_id: MembershipId(uuid::Uuid::from_u128(14)),
            item_id: item_id(),
            project_ref: ProjectRef(host("project", "reading")),
            scope: ScopeRef("private".into()),
            saved_at: Instant::parse("2026-10-22T12:00:00Z").unwrap(),
            created_here_at: at(),
            revision: Revision(1),
            lifecycle: Lifecycle::Active,
            removed_at: None,
            common: Extensions::default(),
        }],
        assignments: vec![
            assignment(21, "design", TagTarget::Item(item_id())),
            assignment(22, "design", project()),
            legacy,
        ],
        targets: vec![
            TargetAccess {
                target: TagTarget::Item(item_id()),
                scope: ScopeRef("private".into()),
                label: "This item".into(),
                writable: true,
            },
            TargetAccess {
                target: project(),
                scope: ScopeRef("private".into()),
                label: "Reading collection".into(),
                writable: true,
            },
            TargetAccess {
                target: folder,
                scope: ScopeRef("private".into()),
                label: "Reference folder".into(),
                writable: false,
            },
        ],
        tag_choices: vec![
            TagChoice {
                tag: tag("design"),
                label: "Design".into(),
            },
            TagChoice {
                tag: tag("reference"),
                label: "Reference".into(),
            },
            TagChoice {
                tag: tag("later"),
                label: "Read later".into(),
            },
            TagChoice {
                tag: tag("other-design"),
                label: "Design".into(),
            },
        ],
        can_edit_metadata: true,
        item_owner: host("item", "1"),
        owner_fences: vec![
            ExpectedRevision {
                owner: host("item", "1"),
                revision: Revision(2),
            },
            ExpectedRevision {
                owner: host("tag-catalogue", "private"),
                revision: Revision(4),
            },
            ExpectedRevision {
                owner: host("project", "reading"),
                revision: Revision(3),
            },
        ],
        bounds: bounds(),
    }
}
pub fn context(id: u128) -> CommandContext {
    CommandContext {
        actor: host("actor", "operator"),
        scope: ScopeRef("private".into()),
        operation_id: OperationId(uuid::Uuid::from_u128(id)),
        intent_digest: String::new(),
        expected_revisions: inspection().owner_fences,
        bounds: bounds(),
    }
}
