use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandContext {
    pub actor: HostRef, pub scope: ScopeRef, pub operation_id: OperationId,
    pub intent_digest: String, #[serde(with="expected_revision_map")] pub expected_revisions: Vec<ExpectedRevision>, pub bounds: Bounds,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemPatch {
    pub title: Patch<Override<String>>, pub description: Patch<Override<String>>,
    pub cover: Patch<Override<MemberId>>, pub selected_observation: Patch<ObservationId>,
    pub gallery_observation: Patch<ObservationId>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureCommand { pub context:CommandContext, pub capture:URLCapture, pub item_id:ItemId }
/// Exact original creation preimage, persisted before stream reservation or observation effects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitialObservation { pub item:Item, pub capture:URLCapture }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentityQuery { pub provider:String, pub namespace:String, pub provider_item_id:String, pub access_partition:ScopeRef }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UrlIdentityQuery { pub identity_url:String, pub normalization_version:String, pub access_partition:ScopeRef }
impl UrlIdentityQuery {
    pub fn validate(&self,bounds:&Bounds)->Result<()> {
        bounds.validate()?;
        if self.access_partition.0.is_empty(){return Err(crate::invalid("access_partition","nonempty scope required"));}
        bounds.text("access_partition",&self.access_partition.0,bounds.max_text_bytes)?;
        bounds.text("normalization_version",&self.normalization_version,bounds.max_text_bytes)?;
        if self.normalization_version!=crate::identity::NORMALIZATION_VERSION {
            return Err(Error::UnsupportedRequiredFeature{feature:"hsscrap.url_normalization_version".into()});
        }
        if crate::identity::normalize_url(&self.identity_url,bounds)?.identity_url!=self.identity_url {
            return Err(crate::invalid("identity_url","normalized URL required"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemSnapshot { pub item:Item, pub captures:Vec<URLCapture>, pub observations:Vec<MetadataObservation>, pub members:Vec<GalleryMember>, pub streams:Vec<ObservationStream> }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page<T> { pub values:Vec<T>, pub next_cursor:Option<String>, pub complete:bool }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectContext { pub project_ref:ProjectRef, pub scope:ScopeRef, pub assignments:Vec<TagAssignment> }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FolderContext { pub folder_ref:FolderRef, pub scope:ScopeRef, pub project:Option<ProjectContext>, pub assignments:Vec<TagAssignment> }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaMetadata { pub media_ref:MediaRef, pub revision:String, pub mime:String, pub measured_width:Option<u32>, pub measured_height:Option<u32>, pub content_hash:String, pub availability:Availability }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaReceipt { pub media_ref:MediaRef, pub receipt_ref:HostRef, pub revision:String }

pub trait ItemOwner {
    fn capture(&mut self, command:CaptureCommand)->Result<OperationOutcome>;
    fn inspect(&self, context:&CommandContext, item:ItemId)->Result<ItemSnapshot>;
    fn lookup_identity(&self, context:&CommandContext, query:IdentityQuery)->Result<Option<ItemId>>;
    /// Return every currently authorized canonical candidate under the stated URL normalization version.
    fn lookup_url(&self, _context:&CommandContext, _query:UrlIdentityQuery)->Result<Vec<ItemId>> {
        Err(Error::UnsupportedRequiredFeature{feature:"hsscrap.identity_index.v1".into()})
    }
    /// Register a verified immutable source assertion through the item owner's revision and operation boundary.
    fn assert_source_identity(&mut self, _context:CommandContext, _identity:SourceIdentity)->Result<OperationOutcome> {
        Err(Error::UnsupportedRequiredFeature{feature:"hsscrap.identity_index.v1".into()})
    }
    fn inspect_source_identity(&self, _context:&CommandContext, _id:IdentityId)->Result<SourceIdentity> {
        Err(Error::UnsupportedRequiredFeature{feature:"hsscrap.identity_index.v1".into()})
    }
    fn apply_patch(&mut self, context:CommandContext, item:ItemId, patch:ItemPatch)->Result<OperationOutcome>;
    fn accept_observation(&mut self, context:CommandContext, observation:MetadataObservation)->Result<OperationOutcome>;
    fn accept_initial_observation(&mut self, _context:CommandContext, _observation:MetadataObservation,
        _initial:InitialObservation)->Result<OperationOutcome> {
        Err(Error::UnsupportedRequiredFeature{feature:"hsscrap.initial_observation.v1".into()})
    }
    fn apply_gallery_snapshot(&mut self, context:CommandContext, item:ItemId, observation:ObservationId)->Result<OperationOutcome>;
    fn tombstone(&mut self, context:CommandContext, item:ItemId)->Result<OperationOutcome>;
    fn operation_outcome(&self, context:&CommandContext, operation:OperationId)->Result<OperationOutcome>;
}
pub trait TagCatalogue {
    fn resolve_id(&self, context:&CommandContext, tag:&TagRef)->Result<TagRef>;
    fn authorized_assignments(&self, context:&CommandContext, target:&TagTarget, cursor:Option<&str>)->Result<Page<TagAssignment>>;
    fn add_assignment(&mut self, context:CommandContext, assignment:TagAssignment)->Result<OperationOutcome>;
    fn remove_assignment(&mut self, context:CommandContext, assignment:AssignmentId)->Result<OperationOutcome>;
}
pub trait ProjectContextOwner {
    fn resolve_project(&self, context:&CommandContext, project:&ProjectRef)->Result<ProjectContext>;
    fn resolve_folder_context(&self, context:&CommandContext, folder:&FolderRef)->Result<FolderContext>;
    fn membership_save(&mut self, context:CommandContext, membership:ProjectMembership)->Result<OperationOutcome>;
    fn membership_remove(&mut self, context:CommandContext, membership:MembershipId)->Result<OperationOutcome>;
    fn placement_add(&mut self, context:CommandContext, placement:FolderPlacement)->Result<OperationOutcome>;
    fn placement_move(&mut self, context:CommandContext, placement:FolderPlacement)->Result<OperationOutcome>;
    fn placement_remove(&mut self, context:CommandContext, placement:PlacementId)->Result<OperationOutcome>;
}
pub trait MediaOwner {
    fn resolve_metadata(&self, context:&CommandContext, media:&MediaRef)->Result<MediaMetadata>;
    fn resolve_tier(&self, context:&CommandContext, media:&MediaRef, tier:RetainedRole)->Result<RemoteTarget>;
    fn materialize(&mut self, context:CommandContext, member:MemberRef, source:HostRef)->Result<MediaReceipt>;
    fn commit_derivative(&mut self, context:CommandContext, original:BindingId, materialization:HostRef)->Result<MediaReceipt>;
}
pub trait MutationCoordinator {
    fn commit(&mut self, context:CommandContext, owner_commands:Vec<OwnerCommand>)->Result<OperationOutcome>;
    fn lookup(&self, context:&CommandContext, operation:OperationId)->Result<OperationOutcome>;
    fn recover(&mut self, context:CommandContext)->Result<OperationOutcome>;
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag="kind",content="value",rename_all="snake_case")]
pub enum OwnerCommand {
    Capture { item:ItemId, capture:URLCapture },
    AcceptObservation { observation:MetadataObservation },
    AcceptInitialObservation { observation:MetadataObservation, initial:InitialObservation },
    ApplyGallerySnapshot { item:ItemId, observation:ObservationId },
    Tombstone { item:ItemId },
    ItemPatch { item:ItemId, patch:ItemPatch }, AddTag(TagAssignment), RemoveTag(AssignmentId),
    AssertSourceIdentity(SourceIdentity),
    SaveMembership(ProjectMembership), RemoveMembership(MembershipId),
    AddPlacement(FolderPlacement), MovePlacement(FolderPlacement), RemovePlacement(PlacementId),
}
