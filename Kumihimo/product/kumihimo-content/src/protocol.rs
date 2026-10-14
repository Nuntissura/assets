use kumihimo_core::{
    EditingTarget, Identity, ProviderId, Rect, ResourceId, Scalar, ToolId, ViewId,
};
use std::collections::BTreeSet;

macro_rules! ids {
    ($($name:ident),+ $(,)?) => {$(
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub u64);
    )+};
}
ids!(MountId, InstanceId, RequestId, CacheId);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid(&'static str),
    IdentityMismatch,
    SessionMismatch,
    Missing(&'static str, u64),
    Unsupported(&'static str),
    Admission {
        dimension: &'static str,
        limit: usize,
        attempted: usize,
    },
    Stale(&'static str),
    Expired(RequestId),
    Conflict(RequestId),
    Busy(InstanceId),
    TimeWentBack,
    Exhausted(&'static str),
    PermissionDenied,
    Core(kumihimo_core::Error),
}
impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Core(error) => Some(error),
            _ => None,
        }
    }
}
impl From<kumihimo_core::Error> for Error {
    fn from(error: kumihimo_core::Error) -> Self {
        Self::Core(error)
    }
}
pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}
impl ProtocolVersion {
    pub const V1: Self = Self { major: 1, minor: 0 };
    pub const V2: Self = Self { major: 2, minor: 0 };
    pub fn validate(self) -> Result<()> {
        if self == Self::V2 {
            Ok(())
        } else {
            Err(Error::Unsupported("protocol version"))
        }
    }
}
impl Default for ProtocolVersion {
    fn default() -> Self {
        Self::V2
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ContentMode {
    Preview,
    QuickEdit,
    FullEditor,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseMode {
    HostDecisionOnly,
    ProviderVeto,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LifecycleState {
    Visible,
    Background,
    Throttled,
    Frozen,
    Unloaded,
    Restoring,
    Failed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RestorationStrength {
    LayoutOnly,
    ProviderDocumentState,
    PageReload,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capabilities {
    #[cfg(feature = "navigation")]
    pub navigation: crate::NavigationCapabilities,
    #[cfg(feature = "backgrounds")]
    pub backgrounds: crate::BackgroundCapabilities,
    pub modes: BTreeSet<ContentMode>,
    pub lifecycle: BTreeSet<LifecycleState>,
    pub restoration: BTreeSet<RestorationStrength>,
    pub sharing: bool,
    pub geometry: bool,
    pub focus: bool,
    pub presentation: bool,
    pub invalidation: bool,
    pub animation: bool,
    pub input: bool,
    pub embed_input: bool,
    pub anchored_reflow: bool,
    pub tools: bool,
    pub assets: bool,
    pub cache: bool,
    pub close_mode: CloseMode,
}
impl Default for Capabilities {
    fn default() -> Self {
        Self {
            #[cfg(feature = "navigation")]
            navigation: Default::default(),
            #[cfg(feature = "backgrounds")]
            backgrounds: Default::default(),
            modes: BTreeSet::from([
                ContentMode::Preview,
                ContentMode::QuickEdit,
                ContentMode::FullEditor,
            ]),
            lifecycle: BTreeSet::from([
                LifecycleState::Visible,
                LifecycleState::Background,
                LifecycleState::Throttled,
                LifecycleState::Frozen,
                LifecycleState::Unloaded,
                LifecycleState::Restoring,
                LifecycleState::Failed,
            ]),
            restoration: BTreeSet::from([
                RestorationStrength::LayoutOnly,
                RestorationStrength::ProviderDocumentState,
                RestorationStrength::PageReload,
                RestorationStrength::Unknown,
            ]),
            sharing: false,
            geometry: true,
            focus: true,
            presentation: true,
            invalidation: true,
            animation: true,
            input: true,
            embed_input: false,
            anchored_reflow: false,
            tools: true,
            assets: true,
            cache: true,
            close_mode: CloseMode::HostDecisionOnly,
        }
    }
}
impl Capabilities {
    pub fn intersect(&self, provider: &Self) -> Result<Self> {
        if self.close_mode != provider.close_mode {
            return Err(Error::Unsupported("close mode"));
        }
        Ok(Self {
            #[cfg(feature = "navigation")]
            navigation: self.navigation.intersect(&provider.navigation),
            #[cfg(feature = "backgrounds")]
            backgrounds: self.backgrounds.intersect(&provider.backgrounds),
            modes: self.modes.intersection(&provider.modes).copied().collect(),
            lifecycle: self
                .lifecycle
                .intersection(&provider.lifecycle)
                .copied()
                .collect(),
            restoration: self
                .restoration
                .intersection(&provider.restoration)
                .copied()
                .collect(),
            sharing: self.sharing && provider.sharing,
            geometry: self.geometry && provider.geometry,
            focus: self.focus && provider.focus,
            presentation: self.presentation && provider.presentation,
            invalidation: self.invalidation && provider.invalidation,
            animation: self.animation && provider.animation,
            input: self.input && provider.input,
            embed_input: self.embed_input && provider.embed_input,
            anchored_reflow: self.anchored_reflow && provider.anchored_reflow,
            tools: self.tools && provider.tools,
            assets: self.assets && provider.assets,
            cache: self.cache && provider.cache,
            close_mode: self.close_mode,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MountTarget {
    View(ViewId),
    #[cfg(feature = "backgrounds")]
    FrameBackground(kumihimo_core::FrameId),
    #[cfg(feature = "backgrounds")]
    WorkspaceBackground(kumihimo_core::WorkspaceId),
}
impl MountTarget {
    pub fn view(self) -> Option<ViewId> {
        match self {
            Self::View(id) => Some(id),
            #[cfg(feature = "backgrounds")]
            _ => None,
        }
    }
    pub fn raw(self) -> u64 {
        match self {
            Self::View(id) => id.raw(),
            #[cfg(feature = "backgrounds")]
            Self::FrameBackground(id) => id.raw(),
            #[cfg(feature = "backgrounds")]
            Self::WorkspaceBackground(id) => id.raw(),
        }
    }
    pub fn is_background(self) -> bool {
        self.view().is_none()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    pub identity: Identity,
    pub session: String,
    pub provider: ProviderId,
    pub resource: ResourceId,
    pub target: MountTarget,
    pub connection: u64,
    pub instance: InstanceId,
    pub mount: MountId,
}
impl Scope {
    pub fn validate(&self) -> Result<()> {
        for token in [
            &self.identity.lineage,
            &self.identity.incarnation,
            &self.session,
        ] {
            if token.is_empty() || token.len() > 128 {
                return Err(Error::Invalid("identity/session token"));
            }
        }
        if [self.provider.raw(), self.resource.raw(), self.target.raw()]
            .iter()
            .any(|id| *id == 0 || *id == u64::MAX)
            || self.connection == 0
            || self.instance.0 == 0
            || self.mount.0 == 0
        {
            return Err(Error::Invalid("scope identity"));
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Channel {
    #[cfg(feature = "navigation")]
    Navigation,
    #[cfg(feature = "backgrounds")]
    MediaBudget,
    Attach,
    Geometry,
    Focus,
    Presentation,
    Redraw,
    Animation,
    Input,
    Tool,
    Asset,
    Lifecycle,
    Close,
    Recovery,
    Cache,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ticket {
    pub scope: Scope,
    pub id: RequestId,
    pub channel: Channel,
    pub revision: u64,
}
impl Ticket {
    pub fn validate(&self) -> Result<()> {
        self.scope.validate()?;
        if self.id.0 == 0 || self.revision == 0 {
            return Err(Error::Invalid("request identity/revision"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub rect: Rect,
    pub clip: Rect,
    pub scale: Scalar,
    pub anchor: Option<String>,
}
impl Geometry {
    pub fn validate(&self) -> Result<()> {
        if !self.scale.get().is_finite() || self.scale.get() <= 0.0 {
            return Err(Error::Invalid("geometry scale"));
        }
        if !self.rect.contains(self.clip) {
            return Err(Error::Invalid("geometry clip"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputOwner {
    FrameChrome,
    Content,
    Embed(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Input {
    pub owner: InputOwner,
    pub payload: String,
    pub x: Option<Scalar>,
    pub y: Option<Scalar>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Observation<T> {
    Known(T),
    Unknown,
}
impl<T> Default for Observation<T> {
    fn default() -> Self {
        Self::Unknown
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Signals {
    pub selected: bool,
    pub visible: bool,
    pub focused: bool,
    pub minimized: bool,
    pub occlusion: Observation<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostAssetReference {
    pub identity: String,
    pub revision: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetMode {
    LinkedEdit,
    IndependentCopy,
    DomainChange,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UndoScope {
    HostDocument,
    HostDomain,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetAction {
    pub asset: HostAssetReference,
    pub mode: AssetMode,
    pub undo: UndoScope,
    pub payload: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostPermission {
    pub allowed: bool,
    pub eligibility_revision: u64,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Eligibility {
    pub revision: u64,
    pub priority: i32,
    pub last_used_ms: u64,
    pub idle_ms: u64,
    pub keep_alive: bool,
    pub unsaved: bool,
    pub audio: bool,
    pub download: bool,
    pub protected: bool,
    pub can_destroy: bool,
    pub recoverable: bool,
    pub host_allowed: bool,
}
impl Eligibility {
    pub fn protected(&self) -> bool {
        self.keep_alive || self.unsaved || self.audio || self.download || self.protected
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestKind {
    #[cfg(feature = "navigation")]
    Navigation(crate::NavigationRequest),
    #[cfg(feature = "backgrounds")]
    MediaBudget(crate::MediaBudgetRequest),
    Attach {
        mode: ContentMode,
        restore_reference: String,
    },
    Detach,
    Dispose,
    Reconcile {
        uncertain: RequestId,
    },
    Geometry(Geometry),
    Focus(bool),
    Presentation(Signals),
    Invalidate,
    Animation {
        deadline: Option<u64>,
    },
    Input(Input),
    ToolAction {
        tool: ToolId,
        target: EditingTarget,
        payload: String,
    },
    AssetAction(AssetAction),
    #[cfg(feature = "drops")]
    Drop(crate::DropRequest),
    Lifecycle(LifecycleState),
    CloseDecision {
        eligibility_revision: u64,
    },
    CacheRelease(CacheId),
}
impl RequestKind {
    pub fn channel(&self) -> Channel {
        match self {
            #[cfg(feature = "navigation")]
            Self::Navigation(_) => Channel::Navigation,
            #[cfg(feature = "backgrounds")]
            Self::MediaBudget(_) => Channel::MediaBudget,
            Self::Attach { .. } | Self::Detach => Channel::Attach,
            Self::Dispose | Self::Lifecycle(_) => Channel::Lifecycle,
            Self::Reconcile { .. } => Channel::Recovery,
            Self::Geometry(_) => Channel::Geometry,
            Self::Focus(_) => Channel::Focus,
            Self::Presentation(_) => Channel::Presentation,
            Self::Invalidate => Channel::Redraw,
            Self::Animation { .. } => Channel::Animation,
            Self::Input(_) => Channel::Input,
            Self::ToolAction { .. } => Channel::Tool,
            Self::AssetAction(_) => Channel::Asset,
            #[cfg(feature = "drops")]
            Self::Drop(_) => Channel::Asset,
            Self::CloseDecision { .. } => Channel::Close,
            Self::CacheRelease(_) => Channel::Cache,
        }
    }
    pub fn barrier(&self) -> bool {
        match self {
            #[cfg(feature = "navigation")]
            Self::Navigation(n) => n.barrier(),
            Self::Geometry(geometry) => geometry.anchor.is_some(),
            Self::Focus(_) | Self::Presentation(_) | Self::Invalidate | Self::Animation { .. } => {
                false
            }
            _ => true,
        }
    }
    pub fn coalescible(&self) -> bool {
        #[cfg(feature = "navigation")]
        if let Self::Navigation(n) = self {
            return !n.barrier();
        }
        matches!(
            self,
            Self::Geometry(Geometry { anchor: None, .. })
                | Self::Focus(_)
                | Self::Presentation(_)
                | Self::Invalidate
        )
    }
    pub fn opaque_bytes(&self) -> Result<usize> {
        match self {
            #[cfg(feature = "drops")]
            Self::Drop(d) => d.offer.opaque_bytes(),
            #[cfg(feature = "navigation")]
            Self::Navigation(n) => n.opaque_bytes(),
            Self::Attach {
                restore_reference, ..
            } => Ok(restore_reference.len()),
            Self::Geometry(geometry) => Ok(geometry.anchor.as_ref().map_or(0, String::len)),
            Self::Input(input) => {
                let recipient = match &input.owner {
                    InputOwner::Embed(recipient) => recipient.len(),
                    _ => 0,
                };
                bytes_sum([input.payload.len(), recipient])
            }
            Self::ToolAction { payload, .. } => Ok(payload.len()),
            Self::AssetAction(action) => bytes_sum([
                action.asset.identity.len(),
                action.asset.revision.len(),
                action.payload.len(),
            ]),
            _ => Ok(0),
        }
    }
    pub fn validate(&self, field_limit: Option<usize>) -> Result<()> {
        match self {
            #[cfg(feature = "drops")]
            Self::Drop(d) => d.validate(field_limit)?,
            #[cfg(feature = "navigation")]
            Self::Navigation(n) => n.validate(field_limit)?,
            #[cfg(feature = "backgrounds")]
            Self::MediaBudget(b) => b.validate()?,
            Self::Attach {
                restore_reference, ..
            } => field(restore_reference, field_limit)?,
            Self::Geometry(geometry) => {
                geometry.validate()?;
                if let Some(anchor) = &geometry.anchor {
                    field(anchor, field_limit)?;
                }
            }
            Self::Input(input) => {
                if input.x.is_some() != input.y.is_some() {
                    return Err(Error::Invalid("input coordinate pair"));
                }
                field(&input.payload, field_limit)?;
                if let InputOwner::Embed(recipient) = &input.owner {
                    if recipient.is_empty() {
                        return Err(Error::Invalid("embed recipient"));
                    }
                    field(recipient, field_limit)?;
                }
            }
            Self::ToolAction { payload, .. } => field(payload, field_limit)?,
            Self::AssetAction(action) => {
                if action.asset.identity.is_empty() || action.asset.revision.is_empty() {
                    return Err(Error::Invalid("asset reference"));
                }
                for value in [
                    &action.asset.identity,
                    &action.asset.revision,
                    &action.payload,
                ] {
                    field(value, field_limit)?;
                }
            }
            Self::Reconcile { uncertain } if uncertain.0 == 0 => {
                return Err(Error::Invalid("uncertain request"))
            }
            Self::CacheRelease(cache) if cache.0 == 0 => {
                return Err(Error::Invalid("cache identity"))
            }
            _ => {}
        }
        self.opaque_bytes()?;
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub ticket: Ticket,
    pub kind: RequestKind,
    pub deadline: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Applied,
    NotApplied,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Confirmation {
    #[cfg(feature = "navigation")]
    Navigation(crate::NavigationObservation),
    #[cfg(feature = "backgrounds")]
    MediaBudget(crate::MediaObservation),
    Done,
    Attached {
        strength: RestorationStrength,
    },
    Geometry {
        anchor: Option<String>,
    },
    Lifecycle(LifecycleState),
    Close {
        approved: bool,
    },
    Reconciled {
        effect: Effect,
        lifecycle: Option<LifecycleState>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AckResult {
    Confirmed(Confirmation),
    Unsupported,
    Rejected,
    Failed { ambiguous: bool },
}
impl AckResult {
    pub fn opaque_bytes(&self) -> Result<usize> {
        Ok(match self {
            Self::Confirmed(Confirmation::Geometry { anchor }) => {
                anchor.as_ref().map_or(0, String::len)
            }
            _ => 0,
        })
    }
    pub fn validate(&self, field_limit: Option<usize>) -> Result<()> {
        #[cfg(feature = "backgrounds")]
        if let Self::Confirmed(Confirmation::MediaBudget(obs)) = self {
            obs.validate()?;
        }
        if let Self::Confirmed(Confirmation::Geometry {
            anchor: Some(anchor),
        }) = self
        {
            field(anchor, field_limit)?;
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ack {
    pub ticket: Ticket,
    pub result: AckResult,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestStatus {
    Queued,
    Dispatched,
    Confirmed,
    Unsupported,
    Rejected,
    Failed,
    Superseded,
    Cancelled,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub ticket: Ticket,
    pub status: RequestStatus,
    pub result: Option<AckResult>,
    pub cause: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentLimits {
    #[cfg(feature = "navigation")]
    pub overview_markers: Option<usize>,
    #[cfg(feature = "navigation")]
    pub overview_entries: Option<usize>,
    #[cfg(feature = "navigation")]
    pub overview_bytes: Option<usize>,
    #[cfg(feature = "navigation")]
    pub marker_bytes: Option<usize>,
    pub providers: Option<usize>,
    pub mounts: Option<usize>,
    pub instances: Option<usize>,
    pub leases: Option<usize>,
    pub pending: Option<usize>,
    pub queue: Option<usize>,
    pub receipts: Option<usize>,
    pub payload_bytes: Option<usize>,
    pub retained_bytes: Option<usize>,
    pub cache_entries: Option<usize>,
    pub cache_bytes: Option<usize>,
    pub ordinary_timeout_ms: u64,
}
impl Default for ContentLimits {
    fn default() -> Self {
        Self {
            #[cfg(feature = "navigation")]
            overview_markers: Some(4096),
            #[cfg(feature = "navigation")]
            overview_entries: Some(65536),
            #[cfg(feature = "navigation")]
            overview_bytes: Some(65536),
            #[cfg(feature = "navigation")]
            marker_bytes: Some(256),
            providers: Some(1024),
            mounts: Some(256),
            instances: Some(64),
            leases: Some(1024),
            pending: Some(1024),
            queue: Some(1024),
            receipts: Some(1024),
            payload_bytes: Some(65_536),
            retained_bytes: Some(16_777_216),
            cache_entries: Some(256),
            cache_bytes: Some(67_108_864),
            ordinary_timeout_ms: 30_000,
        }
    }
}
impl ContentLimits {
    pub const fn unbounded() -> Self {
        Self {
            #[cfg(feature = "navigation")]
            overview_markers: None,
            #[cfg(feature = "navigation")]
            overview_entries: None,
            #[cfg(feature = "navigation")]
            overview_bytes: None,
            #[cfg(feature = "navigation")]
            marker_bytes: None,
            providers: None,
            mounts: None,
            instances: None,
            leases: None,
            pending: None,
            queue: None,
            receipts: None,
            payload_bytes: None,
            retained_bytes: None,
            cache_entries: None,
            cache_bytes: None,
            ordinary_timeout_ms: 30_000,
        }
    }
}
fn field(value: &str, limit: Option<usize>) -> Result<()> {
    if let Some(limit) = limit {
        if value.len() > limit {
            return Err(Error::Admission {
                dimension: "payload_bytes",
                limit,
                attempted: value.len(),
            });
        }
    }
    Ok(())
}
fn bytes_sum<const N: usize>(counts: [usize; N]) -> Result<usize> {
    counts.into_iter().try_fold(0usize, |sum, count| {
        sum.checked_add(count)
            .ok_or(Error::Exhausted("opaque bytes"))
    })
}
