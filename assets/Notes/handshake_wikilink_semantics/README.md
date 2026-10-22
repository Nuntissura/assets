# Wikilink semantics

Internal Rust component of the Notes kit. The parser reads wikilinks and converts supported links to the canonical HsLink model. The resolver provides an in-memory document index and resolves exact reference/ID first, normalized title second, then alias. Duplicate normalized titles or aliases return explicit ambiguity.

Use the parser and resolver modules exported by src/lib.rs. Populate ResolverIndex through add_document and add_alias, then call resolve_wikilink. normalize_target trims, collapses whitespace and lowercases comparison text; original display text is preserved. Alias state is local to the index and does not establish persisted backend alias support. Missing and ambiguous results must remain visible to the caller.

The host retains GUI interaction, authenticated operations, storage and native CreateNote intent handling. This component does not create notes or invent document identities.

The package depends on the canonical sibling `handshake_rich_document_model` crate and returns its `node::HsLinkNode`. The model owns the link type; this package owns parsing and resolution. No host dependency is required.

Run `cargo test --locked` from this package directory to verify parser and resolver behavior, including unknown prefixes, normalized title and alias ambiguity, and exact-reference precedence. Host GUI, backend persistence and pinned adoption require separate integration proof.
