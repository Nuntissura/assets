//! Studio raster interop: the single owner (lead decision R-OWN, 2026-10-14; CX-CAP-001) of PNG / JPEG
//! (later WebP, GIF, BMP) container decode and encode over `&[u8]`.
//!
//! Folio-free: the only Studio edges are `hsk-studio-accord` and `hsk-studio-observe`, so engines (develop)
//! and the harness can consume it without pulling Folio or the media crate. Mapping a decoded raster into a
//! Studio document stays with interop-psd's raster proposal and the harness route. JPEG export uses a
//! clean-room baseline encoder (decision J1). This crate is a skeleton until the SMB-IO-RASTER-COMMON lane lands.
