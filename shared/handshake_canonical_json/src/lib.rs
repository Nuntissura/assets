//! Canonical bytes and SHA-256 for portable owner contracts.
//!
//! `canonical_json_bytes` preserves the existing legacy byte encoding, including its historical
//! control-character and number behavior. It is not RFC8785. Existing stored legacy hashes retain
//! that API. `jcs_bytes` and `jcs_from_slice` are separate RFC8785/JCS entrypoints for new contracts.
//! Hosts own signatures, trust, audience, schema validation, numeric policy and storage.
//!
//! JCS uses IEEE754 binary64 numbers. Integers larger than 9007199254740991 in magnitude can round;
//! counters, revisions and other exact large integers must use decimal strings in their schema.
//! Pass original typed values to `jcs_bytes` so nonfinite floats reject before a conversion to
//! `serde_json::Value` could erase them. Typed serialization trusts an already bounded value and its
//! Serialize implementation; it does not cap output allocation. Callers validate aggregate payload
//! and extension limits before calling it. `jcs_from_slice` bounds raw input, rejects duplicate object
//! keys, malformed JSON, lone surrogates and trailing data, then uses the same JCS serializer.
//! Raw admission builds a literal Value tree in one duplicate-aware bounded parse, preserving reserved
//! object keys under serde_json/raw_value. A separate Number-key probe rejects arbitrary_precision,
//! whose synthetic number maps cannot be distinguished from literal object keys by this visitor.
//! Typed serialization retains the upstream serializer's behavior under dependency features.
//! Neither API normalizes Unicode or supplies transferable grants.
//!
//! Standalone proof from repository root:
//! `cargo test --locked --manifest-path shared/handshake_canonical_json/Cargo.toml`.
//! Run the full suite also with `--features serde_json/raw_value`, then with
//! `--features serde_json/raw_value,serde_json/float_roundtrip`. Probe the unsupported feature with
//! `--features serde_json/arbitrary_precision jcs_tests::raw_parser_feature_behavior_is_explicit`.
//! No services, credentials or GUI are required. Errors identify invalid JSON/serialization or the
//! configured input-byte limit; callers retain source data and must not accept a failed hash.
//!
//! Research inspected 2026-10-23: [RFC8785](https://www.rfc-editor.org/rfc/rfc8785.html), exact
//! [serde_json_canonicalizer 0.4.1](https://crates.io/crates/serde_json_canonicalizer/0.4.1) source,
//! manifest and RFC vectors, and serde_json 1.0.154 parser source matching this standalone lockfile.
//! Release checksum: `87f7bb64f8c7e9f783fdfe883b5fe93a90bee1597c63705baa91842c5f698620`.
//! Reuse the maintained UTF16 ordering and ryu-js number serializer. Reject rewriting the legacy
//! encoder, implementing new floating-point formatting, and using upstream `pipe`, which parses
//! duplicate keys into a Value before encoding. A duplicate-aware Serde visitor handles raw JSON
//! admission instead. RFC byte/number vectors and negative admission tests validate this choice;
//! dependent transfer schemas must separately validate precise integers and authorized projections.

pub mod canonical_json;
mod strict_jcs;

pub use canonical_json::{canonical_json_bytes, sha256_hex};
pub use strict_jcs::{jcs_bytes, jcs_from_slice};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod jcs_tests;
