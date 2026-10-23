//! Identity minting is caller-owned: the importer never invents ids on its own.

/// Mints a fresh `uuid_v7`-shaped id with the given Studio prefix (`SDOC`, `SPGS`, `SLYR`, `STXT`,
/// `STYS`). Folio validates each id on use.
pub trait IdMint {
    fn mint(&mut self, prefix: &'static str) -> String;
}

/// Deterministic synthetic ids (not time-ordered): the same namespace and call order always yield
/// the same ids. A host should rewrite them on adoption (STU-IO-190).
#[derive(Clone, Debug)]
pub struct SequentialIdMint {
    namespace: u32,
    next: u64,
}

impl SequentialIdMint {
    pub fn new(namespace: u32) -> Self {
        Self { namespace, next: 1 }
    }
}

impl IdMint for SequentialIdMint {
    fn mint(&mut self, prefix: &'static str) -> String {
        let n = self.next;
        self.next += 1;
        format!("{prefix}-{:08x}-0000-7000-8000-{n:012x}", self.namespace)
    }
}
