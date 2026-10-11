macro_rules! ids {
    ($($name:ident),+ $(,)?) => {$ (
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(u64);
        impl $name {
            /// Construct a target reference; this does not create or reserve an entity.
            pub const fn from_raw(raw: u64) -> Self { Self(raw) }
            pub const fn raw(self) -> u64 { self.0 }
        }
    )+};
}
ids!(
    WorkspaceId,
    FrameId,
    ViewId,
    ResourceId,
    ProviderId,
    GroupId
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntityId {
    Workspace(WorkspaceId),
    Frame(FrameId),
    View(ViewId),
    Resource(ResourceId),
    Provider(ProviderId),
    Group(GroupId),
}

/// Operation IDs are sequences scoped to one Core instance, shared by all actors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct OperationId(u64);
impl OperationId {
    pub const fn new(sequence: u64) -> Self {
        Self(sequence)
    }
    pub const fn sequence(self) -> u64 {
        self.0
    }
    pub fn checked_next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}
