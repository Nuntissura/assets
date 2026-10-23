use super::*;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub item_id: ItemId,
    pub owner_scope: ScopeRef,
    pub revision: Revision,
    pub first_capture_id: CaptureId,
    #[serde(deserialize_with="required_option")]
    pub selected_observation_id: Option<ObservationId>,
    #[serde(deserialize_with="required_option")]
    pub gallery_observation_id: Option<ObservationId>,
    pub member_order: Vec<MemberId>,
    pub title_override: Override<String>,
    pub description_override: Override<String>,
    pub cover_override: Override<MemberId>,
    pub created_at: Instant,
    pub updated_at: Instant,
    pub lifecycle: Lifecycle,
    #[serde(deserialize_with="required_option")]
    pub deleted_at: Option<Instant>,
    #[serde(flatten)] pub common: Extensions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct URLCapture {
    pub capture_id: CaptureId,
    pub item_id: ItemId,
    pub capture_kind: CaptureKind,
    pub access_partition: ScopeRef,
    pub captured_url: String,
    #[serde(deserialize_with="required_option")]
    pub original_url: Option<String>,
    pub identity_url: String,
    pub normalization_version: String,
    #[serde(deserialize_with="required_option")]
    pub captured_at: Option<Instant>,
    pub created_here_at: Instant,
    #[serde(deserialize_with="required_option")]
    pub actor_ref: Option<HostRef>,
    pub operation_id: OperationId,
    #[serde(flatten)] pub common: Extensions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceIdentity {
    pub identity_id: IdentityId,
    pub item_id: ItemId,
    pub provider: String,
    pub namespace: String,
    pub provider_item_id: String,
    pub access_partition: ScopeRef,
    pub verified_by_observation_id: ObservationId,
    #[serde(deserialize_with="required_option")]
    pub retired_at: Option<Instant>,
    #[serde(flatten)] pub common: Extensions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetadataObservation {
    pub observation_id: ObservationId,
    pub item_id: ItemId,
    pub stream_id: StreamId,
    pub sequence: Revision,
    #[serde(deserialize_with="required_option")]
    pub baseline_observation_id: Option<ObservationId>,
    #[serde(deserialize_with="required_option")]
    pub capture_id: Option<CaptureId>,
    #[serde(deserialize_with="required_option")]
    pub appearance_ref: Option<HostRef>,
    pub producer: String,
    pub access_partition: ScopeRef,
    pub observed_at: Instant,
    #[serde(deserialize_with="required_option")]
    pub source_revision: Option<String>,
    #[serde(deserialize_with="required_option")]
    pub canonical_url_assertion: Option<String>,
    #[serde(deserialize_with="required_option")]
    pub provider: Option<String>,
    pub host_site: String,
    #[serde(deserialize_with="required_option")]
    pub title: Option<String>,
    #[serde(deserialize_with="required_option")]
    pub description: Option<String>,
    #[serde(deserialize_with="required_option")]
    pub author: Option<String>,
    #[serde(deserialize_with="required_option")]
    pub published_at: Option<PublicationTime>,
    pub provider_labels: Vec<String>,
    #[serde(deserialize_with="required_option")]
    pub gallery_snapshot: Option<GallerySnapshot>,
    #[serde(deserialize_with="required_option")]
    pub source_payload_digest: Option<String>,
    pub status: Availability,
    #[serde(flatten)] pub common: Extensions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GalleryMember {
    pub member_id: MemberId,
    pub item_id: ItemId,
    #[serde(deserialize_with="required_option")]
    pub producer_member_key: Option<String>,
    pub observation_id: ObservationId,
    pub media_kind: MediaKind,
    #[serde(deserialize_with="required_option")]
    pub caption: Option<String>,
    #[serde(deserialize_with="required_option")]
    pub observed_mime: Option<String>,
    #[serde(deserialize_with="required_option")]
    pub observed_width: Option<u32>,
    #[serde(deserialize_with="required_option")]
    pub observed_height: Option<u32>,
    #[serde(with="optional_u64")]
    pub observed_duration_ms: Option<u64>,
    pub availability: Availability,
    pub lifecycle: MemberLifecycle,
    #[serde(deserialize_with="required_option")]
    pub removed_at: Option<Instant>,
    #[serde(flatten)] pub common: Extensions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteMediaReference {
    pub reference_id: HostRef,
    pub member_ref: MemberRef,
    pub observation_id: ObservationId,
    pub role: RemoteRole,
    pub target: RemoteTarget,
    #[serde(deserialize_with="required_option")]
    pub expires_at: Option<Instant>,
    pub access_partition: ScopeRef,
    pub availability: Availability,
    #[serde(flatten)] pub common: Extensions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetainedMediaBinding {
    pub binding_id: BindingId,
    pub member_ref: MemberRef,
    pub media_ref: MediaRef,
    pub media_revision_ref: String,
    pub role: RetainedRole,
    pub materialization_receipt_ref: HostRef,
    pub created_at: Instant,
    #[serde(deserialize_with="required_option")]
    pub derived_from_binding_id: Option<BindingId>,
    #[serde(flatten)] pub common: Extensions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TagAssignment {
    pub assignment_id: AssignmentId,
    pub tag_ref: TagRef,
    pub target: TagTarget,
    pub scope: ScopeRef,
    pub origin: TagOrigin,
    pub created_at: Instant,
    #[serde(deserialize_with="required_option")]
    pub removed_at: Option<Instant>,
    pub revision: Revision,
    #[serde(flatten)] pub common: Extensions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectMembership {
    pub membership_id: MembershipId,
    pub item_id: ItemId,
    pub project_ref: ProjectRef,
    pub scope: ScopeRef,
    pub saved_at: Instant,
    pub created_here_at: Instant,
    pub revision: Revision,
    pub lifecycle: Lifecycle,
    #[serde(deserialize_with="required_option")]
    pub removed_at: Option<Instant>,
    #[serde(flatten)] pub common: Extensions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FolderPlacement {
    pub placement_id: PlacementId,
    pub item_id: ItemId,
    #[serde(deserialize_with="required_option")]
    pub membership_id: Option<MembershipId>,
    #[serde(deserialize_with="required_option")]
    pub folder_ref: Option<FolderRef>,
    pub scope: ScopeRef,
    #[serde(deserialize_with="required_option")]
    pub sort_order: Option<i32>,
    pub placed_at: Instant,
    #[serde(deserialize_with="required_option")]
    pub moved_at: Option<Instant>,
    pub created_here_at: Instant,
    pub revision: Revision,
    pub lifecycle: Lifecycle,
    #[serde(deserialize_with="required_option")]
    pub removed_at: Option<Instant>,
    #[serde(flatten)] pub common: Extensions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OccurrenceReference {
    pub occurrence_ref: OccurrenceRef,
    pub item_id: ItemId,
    #[serde(deserialize_with="required_option")]
    pub member_id: Option<MemberId>,
    #[serde(deserialize_with="required_option")]
    pub context_membership_id: Option<MembershipId>,
    pub caption_override: Override<String>,
    #[serde(deserialize_with="required_option")]
    pub presentation: Option<Presentation>,
    pub revision: Revision,
    #[serde(flatten)] pub common: Extensions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportMapping {
    pub destination_scope: ScopeRef,
    pub origin_instance_id: String,
    pub origin_kind: String,
    pub origin_id: String,
    pub local_ref: HostRef,
    #[serde(with="accepted_version_map")]
    pub accepted_versions: Vec<AcceptedStreamVersion>,
    pub local_revision_at_import: Revision,
    pub imported_at: Instant,
    pub state: ImportState,
    #[serde(flatten)] pub common: Extensions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationOutcome {
    pub operation_id: OperationId,
    pub scope: ScopeRef,
    pub intent_digest: String,
    #[serde(with="expected_revision_map")]
    pub expected_revisions: Vec<ExpectedRevision>,
    pub outcome: OutcomeState,
    pub result_refs: Vec<HostRef>,
    pub owner_receipt_refs: Vec<HostRef>,
    #[serde(deserialize_with="required_option")]
    pub committed_at: Option<Instant>,
    #[serde(flatten)] pub common: Extensions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservationStream {
    pub stream_id: StreamId,
    pub item_id: ItemId,
    pub producer: String,
    pub provider_namespace: String,
    pub source_ref: HostRef,
    pub access_partition: ScopeRef,
    pub last_issued_sequence: Revision,
    #[serde(deserialize_with="required_option")]
    pub last_accepted_sequence: Option<Revision>,
    #[serde(deserialize_with="required_option")]
    pub latest_observation_id: Option<ObservationId>,
    pub revision: Revision,
    #[serde(flatten)] pub common: Extensions,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberObservation {
    pub member_id: MemberId,
    #[serde(deserialize_with="required_option")]
    pub producer_member_key: Option<String>,
    pub media_kind: MediaKind,
    #[serde(deserialize_with="required_option")]
    pub caption: Option<String>,
    #[serde(deserialize_with="required_option")]
    pub observed_mime: Option<String>,
    #[serde(deserialize_with="required_option")]
    pub observed_width: Option<u32>,
    #[serde(deserialize_with="required_option")]
    pub observed_height: Option<u32>,
    #[serde(with="optional_u64")] pub observed_duration_ms: Option<u64>,
    pub availability: Availability,
    pub remote_reference_ids: Vec<HostRef>,
    #[serde(flatten)] pub common: Extensions,
}
