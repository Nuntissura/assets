# Ferric Forager diagnostic contract

`fforager-diagnostics-contract` is the data-only boundary shared by Ferric
diagnostic producers and the independent watcher. It contains no process,
filesystem, network, Tokio, or watcher-runtime handles.

The wire contract uses protocol major/minor negotiation. A major mismatch is
incompatible. A minor overlap selects the greatest mutually supported minor.
Exact schema identity is always accepted. Different schema identities require
an immutable crate-local reviewed transition whose unique review ID and
revision bind one directed source/target pair, an executable validated-semantics
proof ID, and the applicable protocol major/minor range. Peer offers and public
callers cannot construct that authority. The shipped registry is empty, so the
production/default behavior is strict and fail-closed. The legacy V1 peer
boolean is retained only for explicit migration observability and grants no
authority.

`ProtocolOffer` keeps its fields private: `new` validates ranges and schema
sets, and `negotiate` revalidates both inputs before making a compatibility
decision. Rust callers therefore cannot bypass offer invariants by struct
construction.

`NegotiatedProtocol` and `SchemaDisposition` are serialize-only local
negotiation outputs. A reviewed-transition result records its review identity,
proof identity, exact directed schemas, and protocol applicability, but cannot
be deserialized back into compatibility authority. Schema hashes are lowercase
SHA-256 hexadecimal over the canonical schema input identified by
`canonical_input_version`.

All identifiers, strings, collections, frames, and opaque unknown values are
bounded. Deserialization runs the same validation as constructors. Secret data
must be producer-redacted before framing. Unknown optional fields may be held
only as non-exportable bounded opaque values; unknown mandatory kinds fail.

Event sequence identity is `(producer_instance, boot_session, channel,
sequence)`. Sequences start at one and never wrap. The tracker rejects gaps,
duplicates, reordering, cross-stream identity changes, and durable
acknowledgements ahead of admitted data. Explicit reconnect replay is bounded
by a caller-supplied replay window.

The watcher lifecycle distinguishes local `Ready` from Ferric-origin
`Serving`. Health snapshots expose bounded queue counters, last admitted and
durable sequence identities, typed lifecycle state, and every loop's
Idle/Working/Blocked/Failed state. Crash evidence remains retained until an
explicit collection acknowledgement or governed expiry transition.

Run package tests from the repository root after the workspace registers this
package:

```text
cargo test --manifest-path build/Cargo.toml --locked -p fforager-diagnostics-contract
```

