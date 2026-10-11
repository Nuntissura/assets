//! Internal bounded read-only Lightroom .lrcat file-byte decoder, fixed Lightroom semantic
//! extraction only; explicitly granted stable main/WAL handles. No engine, query API, writable
//! catalog or DB. Outputs CKC/Studio proposals through shared ports.
#![forbid(unsafe_code)]
