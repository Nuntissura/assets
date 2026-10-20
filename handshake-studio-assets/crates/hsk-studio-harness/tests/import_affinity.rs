//! SMB-AFFINITY-INTEROP-1 focused tests (feature `import`): Affinity native files (`.af`, `.afphoto`,
//! `.afdesign`, `.afpub`; KA container) are recognised by magic and refused with a typed code that names
//! Affinity's own PSD/PDF/SVG export route (lead decision 2026-10-16, AF-02). Headers are synthetic
//! (layout: research/app-inspect-affinity.md and the MIT reader VMDevCpp/afread); no Affinity-written
//! file is read here, so real-file behaviour is UNVERIFIED (SMB-AFFINITY-ORACLE-OPERATOR).
#![cfg(feature = "import")]

use hsk_studio_accord::CancellationToken;
use hsk_studio_folio as folio;
use hsk_studio_harness::cli;
use hsk_studio_harness::import::{AffinityHeader, ImportError, SourceFormat, affinity_refusal, import, sniff};
use serde_json::Value;
use std::path::PathBuf;

/// A KA header: magic, u16 version, u16 flags, class 4CC stored byte-reversed, then `#Inf` and padding.
fn ka_header(version: u16, flags: u16, class: &[u8; 4]) -> Vec<u8> {
    let mut b = vec![0x00, 0xFF, 0x4B, 0x41];
    b.extend_from_slice(&version.to_le_bytes());
    b.extend_from_slice(&flags.to_le_bytes());
    b.extend(class.iter().rev());
    b.extend_from_slice(b"#Inf");
    b.resize(0x48, 0);
    b
}

#[test]
fn affinity_documents_are_refused_naming_the_export_route() {
    let cancel = CancellationToken::default();
    for (version, flags, generation) in [(9u16, 0u16, "v1"), (11, 0, "v2"), (12, 4, "v3"), (12, 0x200, "v3"), (13, 0, "unknown")] {
        let bytes = ka_header(version, flags, b"Prsn");
        assert_eq!(sniff(&bytes), Some(SourceFormat::Affinity));
        let header = AffinityHeader::probe(&bytes).unwrap();
        assert_eq!((header.version, header.flags, header.generation(), header.is_document()), (version, flags, generation, true));
        let err = import(&bytes, &TrustAllResolver, &cancel).err().expect("a native Affinity file is refused, never imported");
        assert_eq!(err.code(), "affinity_native_use_export");
        assert!(matches!(err, ImportError::UseInterchange { format: SourceFormat::Affinity, .. }));
        let detail = err.detail();
        assert_eq!(detail["format"], "affinity");
        assert_eq!(detail["export_routes"], serde_json::json!(["psd", "pdf", "svg"]));
        assert_eq!(detail["generation"], generation);
        assert_eq!(detail["container_version"], version);
        assert_eq!(detail["class"], "Prsn");
        assert_eq!(detail["layers_decoded"], false);
        assert!(detail["route"].as_str().unwrap().contains("PSD"), "{detail}");
    }
    // A KA resource (studio preset `prss`, stored `ssrp`) is not a document; a magic-only file is truncated.
    let preset = ka_header(12, 0, b"prss");
    assert_eq!(affinity_refusal(&preset).code(), "affinity_resource_not_document");
    assert_eq!(import(&preset, &TrustAllResolver, &cancel).err().unwrap().detail()["class"], "prss");
    let stub = [0x00, 0xFF, 0x4B, 0x41, 0x0C];
    assert_eq!(sniff(&stub), Some(SourceFormat::Affinity));
    let err = import(&stub, &TrustAllResolver, &cancel).err().unwrap();
    assert_eq!((err.code(), err.detail()["generation"].clone()), ("affinity_header_truncated", Value::Null));
    // Near-misses are not Affinity.
    assert_eq!(sniff(&[0x00, 0xFF, 0x4B, 0x53, 0, 0, 0, 0]), None);
    assert_eq!(sniff(&[0xFF, 0x00, 0x4B, 0x41, 0, 0, 0, 0]), None);
}

#[test]
fn cli_import_of_an_affinity_file_reports_the_route_and_writes_nothing() {
    let dir = scratch("cli");
    let source = dir.join("doc.afdesign");
    std::fs::write(&source, ka_header(11, 0, b"Prsn")).unwrap();
    let saved = dir.join("out.handshake");
    let args: Vec<String> =
        ["import", source.to_str().unwrap(), "--save", saved.to_str().unwrap()].iter().map(|s| (*s).to_string()).collect();
    let out = cli::run(&args);
    let report: Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(out.exit, 1, "{report}");
    assert_eq!(report["ops"][0]["error"], "affinity_native_use_export", "{report}");
    assert_eq!(report["ops"][0]["detail"]["import"]["export_routes"][0], "psd", "{report}");
    assert!(!saved.exists(), "a refused import writes no package");
    let _ = std::fs::remove_dir_all(&dir);
}

fn scratch(label: &str) -> PathBuf {
    let base = std::env::temp_dir();
    assert!(!(cfg!(windows) && base.to_string_lossy().to_ascii_uppercase().starts_with("C:")), "TMP is on C:");
    let dir = base.join(format!("hsk-harness-affinity-{}-{label}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct TrustAllResolver;
impl folio::Resolver for TrustAllResolver {
    fn primitive(&self, _: &str, _: &str) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn profile(&self, _: &str) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn tile(&self, _: &folio::TileRef) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
}

