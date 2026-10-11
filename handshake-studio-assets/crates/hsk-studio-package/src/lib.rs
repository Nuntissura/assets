//! Native document container serialization, manifest/hash references, atomic recovery and bounded
//! loading. Stream asset bytes through CKC/PRIM-ArtifactService provider ports; no private catalog,
//! preview cache, metadata store, watcher or second CAS under STU-ASSET-004/009/010. Standalone
//! fixtures supply scoped in-memory resolver inputs rather than kernel imports.
#![forbid(unsafe_code)]
