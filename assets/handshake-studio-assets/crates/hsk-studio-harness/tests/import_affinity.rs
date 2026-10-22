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

// ---- slice 3: opt-in thumbnail preview (feature import-affinity) ------------------------------------

/// A V3 KA document (`#FT4`, one stored `doc.dat`, optional `Thmb` block) from the documented layout.
#[cfg(feature = "import-affinity")]
fn ka_document(thumbnail: Option<&[u8]>) -> Vec<u8> {
    let doc = b"\x00\xFFKS synthetic object stream (not decoded)".to_vec();
    let mut b = ka_header(12, 4, b"Prsn");
    b[0x40..0x44].copy_from_slice(b"Prot");
    let stream_at = b.len() as u64;
    b.extend_from_slice(b"#Fil");
    b.extend_from_slice(&doc);
    let dir_at = b.len() as u64;
    b.extend_from_slice(b"#FT4");
    b.extend_from_slice(&0u64.to_le_bytes());
    b.extend_from_slice(&1_760_000_000u64.to_le_bytes());
    b.extend_from_slice(&[0u8; 24]);
    b.extend_from_slice(&1u32.to_le_bytes());
    b.extend_from_slice(&[0u8; 8]);
    b.extend_from_slice(&0u16.to_le_bytes());
    b.push(0);
    b.extend_from_slice(&1u32.to_le_bytes());
    b.push(0);
    b.extend_from_slice(&stream_at.to_le_bytes());
    b.extend_from_slice(&(doc.len() as u64).to_le_bytes());
    b.extend_from_slice(&(doc.len() as u64).to_le_bytes());
    // CRC-32 of `doc`, computed once with Python zlib.crc32 (no CRC copy in tests, V4-05).
    b.extend_from_slice(&0x02E4_7161u32.to_le_bytes());
    b.push(0); // stored
    b.extend_from_slice(&[0u8; 8]);
    b.extend_from_slice(&7u16.to_le_bytes());
    b.extend_from_slice(b"doc.dat");
    let thumb_at = b.len() as u64;
    b[0x10..0x18].copy_from_slice(&dir_at.to_le_bytes());
    b[0x18..0x20].copy_from_slice(&thumb_at.to_le_bytes());
    if let Some(img) = thumbnail {
        b.extend_from_slice(&[0xFF; 4]);
        b.extend_from_slice(b"Thmb");
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&((img.len() + 13) as u32).to_le_bytes());
        b.extend_from_slice(&29u32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&(img.len() as u32).to_le_bytes());
        b.push(1);
        b.extend_from_slice(img);
    }
    b
}

#[cfg(feature = "import-affinity")]
fn loss_codes(receipt: &Value) -> Vec<String> {
    receipt["loss"].as_array().unwrap().iter().map(|e| e["code"].as_str().unwrap().to_string()).collect()
}

#[cfg(feature = "import-affinity")]
#[test]
fn affinity_preview_imports_the_thumbnail_as_one_lossy_layer_with_the_original() {
    use hsk_studio_harness::import::import_affinity_preview;
    use hsk_studio_interop_raster as raster;
    use hsk_studio_package::{ImportRole, Limits, read_package};
    let cancel = CancellationToken::default();
    let samples = raster::Samples::U8((0..6 * 4 * 4).map(|i| (i * 37 % 256) as u8).collect());
    let req = raster::PngEncodeRequest {
        width: 6,
        height: 4,
        colour: raster::Colour::Rgba,
        samples: &samples,
        icc: None,
        compression: raster::PngCompression::Balanced,
    };
    let png = raster::encode_png(&req).unwrap();
    let bytes = ka_document(Some(&png));
    // The default import still refuses; the preview is an explicit choice.
    assert_eq!(import(&bytes, &TrustAllResolver, &cancel).err().unwrap().code(), "affinity_native_use_export");
    let imported = import_affinity_preview(&bytes, &TrustAllResolver, &cancel).expect("preview imported");
    let receipt = &imported.receipt;
    assert_eq!((receipt["source_format"].as_str(), receipt["layers"].as_u64()), (Some("affinity_preview"), Some(1)), "{receipt}");
    let a = &receipt["affinity"];
    assert_eq!((a["lossy"].as_bool(), a["preview_only"].as_bool(), a["layers_decoded"].as_bool()), (Some(true), Some(true), Some(false)), "{a}");
    assert_eq!((a["generation"].as_str(), a["thumbnail"]["width"].as_u64(), a["thumbnail"]["height"].as_u64()), (Some("v3"), Some(6), Some(4)));
    assert_eq!(a["streams"][0]["name"], "doc.dat");
    assert_eq!(a["carried"][0]["kind"], "object_stream");
    assert_eq!(a["export_routes"], serde_json::json!(["psd", "pdf", "svg"]));
    let codes = loss_codes(receipt);
    assert!(codes.iter().any(|c| c == "affinity_layers_not_decoded"), "{codes:?}");
    assert!(codes.iter().any(|c| c == "affinity_preview_resolution"), "{codes:?}");
    // The package carries the original .af bytes and reopens with one layer.
    let package = imported.to_package(&cancel).unwrap();
    let opened = read_package(&package, &Limits::default(), folio::Budget::default(), &TrustAllResolver, &cancel).unwrap();
    assert_eq!(opened.import_bytes(ImportRole::Original), Some(&bytes[..]));
    assert_eq!(opened.snapshot().map(|s| s.document().layers.len()), Some(1));
    // Typed refusals: no Thmb block, a resource container, a corrupt preview, non-Affinity bytes.
    let code = |b: &[u8]| import_affinity_preview(b, &TrustAllResolver, &cancel).err().map(|e| e.code());
    assert_eq!(code(&ka_document(None)), Some("affinity_thumbnail_absent"));
    let mut preset = ka_document(Some(&png));
    preset[8..12].copy_from_slice(b"ssrp");
    assert_eq!(code(&preset), Some("affinity_resource_not_document"));
    let mut bad = ka_document(Some(&png));
    let n = bad.len();
    bad.truncate(n - 6); // the PNG loses its IEND tail: the chunk walk refuses it
    let block_len_at = n - png.len() - 29 + 12;
    let len = u32::from_le_bytes(bad[block_len_at..block_len_at + 4].try_into().unwrap()) - 6;
    bad[block_len_at..block_len_at + 4].copy_from_slice(&len.to_le_bytes());
    assert_eq!(code(&bad), Some("affinity_thumbnail_image_invalid"));
    assert_eq!(code(&png), Some("format_unrecognised"));
}

/// The one corpus-root convention (`.config/oracle-corpus.toml`): walk up from the workspace root.
#[cfg(feature = "import-affinity")]
fn corpus_root() -> Option<PathBuf> {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let config = std::fs::read_to_string(workspace.join(".config/oracle-corpus.toml")).ok()?;
    let value = |key: &str| {
        config.lines().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
        })
    };
    let search = value("search")?;
    let max: usize = value("max-ancestors")?.parse().ok()?;
    let mut dir = workspace.canonicalize().ok()?;
    for _ in 0..=max {
        let candidate = dir.join(&search);
        if candidate.is_dir() {
            return Some(candidate);
        }
        dir = dir.parent()?.to_path_buf();
    }
    None
}

#[cfg(feature = "import-affinity")]
#[test]
#[ignore = "needs oracle-corpus/affinity/afdesignload-mit (AFDesignLoad MIT V1 test designs)"]
fn afdesignload_v1_designs_open_as_flattened_previews() {
    use hsk_studio_harness::import::import_affinity_preview;
    let Some(root) = corpus_root() else {
        println!("NOT_PROVEN: afdesignload_previews corpus reason=oracle-corpus root not found via .config/oracle-corpus.toml");
        return;
    };
    let dir = root.join("affinity/afdesignload-mit");
    let names = ["color", "layer_mode", "layer_test", "margins", "raster_test", "revision_test", "shape_test", "slice_test", "test_path"];
    let cancel = CancellationToken::default();
    let mut opened = 0;
    for name in names {
        let Ok(bytes) = std::fs::read(dir.join(format!("{name}.afdesign"))) else {
            println!("NOT_PROVEN: afdesignload_previews {name} reason=missing (sha256 pins in provenance.json, checked by interop-affinity tests/container.rs)");
            continue;
        };
        assert_eq!(import(&bytes, &TrustAllResolver, &cancel).err().unwrap().code(), "affinity_native_use_export", "{name}");
        let imported = import_affinity_preview(&bytes, &TrustAllResolver, &cancel).unwrap_or_else(|e| panic!("{name}: {e} {}", e.detail()));
        let a = &imported.receipt["affinity"];
        assert_eq!((a["generation"].as_str(), a["thumbnail"]["width"].as_u64(), a["thumbnail"]["height"].as_u64()), (Some("v1"), Some(512), Some(512)), "{name}");
        assert!(loss_codes(&imported.receipt).iter().any(|c| c == "affinity_layers_not_decoded"), "{name}");
        opened += 1;
    }
    println!("afdesignload_previews opened={opened}");
}
