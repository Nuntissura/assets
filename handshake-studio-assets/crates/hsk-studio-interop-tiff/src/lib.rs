//! Studio TIFF interop: the single TIFF container owner (lead decision F1, 2026-10-14; CX-CAP-001).
//!
//! Owns TIFF container parsing, decoding and encoding: IFD chain, SubIFDs, BigTIFF, strips and tiles,
//! decompression, predictors, sample formats, ICC (tag 34675), ExtraSamples, multi-page, and raw access to
//! the Photoshop tags 37724 (ImageSourceData) and 34377 (ImageResources).
//!
//! Folio-free: the only Studio edges are `hsk-studio-accord` and `hsk-studio-observe`, so the develop engine
//! can consume this crate without gaining a Folio path. Mapping a decoded TIFF into a Studio document belongs
//! to the convert-to-native flow, and the layer records inside tag 37724 are parsed by interop-psd.
//! This crate is a skeleton until the SMB-INTEROP-TIFF lane lands.
