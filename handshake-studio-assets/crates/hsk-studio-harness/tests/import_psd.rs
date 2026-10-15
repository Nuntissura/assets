//! SMB-CONVERT-NATIVE CN-3 focused tests: open directly / convert to native for PSD (feature
//! `import-psd`), typed refusals for unknown and not-yet-importable formats, and the CLI flow. All
//! headless; PSD inputs are synthetic bytes built here (independent of the importer's encoder). The
//! psd-tools corpus sweep is `#[ignore]`d (external test DATA through `.config/oracle-corpus.toml`).
#![cfg(feature = "import-psd")]

use hsk_studio_accord::CancellationToken;
use hsk_studio_folio as folio;
use hsk_studio_harness::import::{ImportError, SourceFormat, convert_to_native, import, sniff};
use hsk_studio_harness::{Harness, References, Selection, cli, default_actor};
use hsk_studio_package::{ImportRole, Limits, read_package};
use serde_json::Value;
use std::path::{Path, PathBuf};

// ---- independent PSD byte assembly (RGB, 8-bit, raw channels) --------------------------------

struct L {
    name: &'static str,
    rect: [i32; 4],
    channels: Vec<(i16, Vec<u8>)>,
}

fn psd(width: u32, height: u32, layers: &[L], merged: &[u8]) -> Vec<u8> {
    let mut v = b"8BPS".to_vec();
    v.extend(1u16.to_be_bytes());
    v.extend([0u8; 6]);
    v.extend(3u16.to_be_bytes());
    v.extend(height.to_be_bytes());
    v.extend(width.to_be_bytes());
    v.extend(8u16.to_be_bytes());
    v.extend(3u16.to_be_bytes());
    v.extend(0u32.to_be_bytes()); // colour mode data
    v.extend(0u32.to_be_bytes()); // image resources
    let mut section = Vec::new();
    if !layers.is_empty() {
        let mut info = (layers.len() as i16).to_be_bytes().to_vec();
        for l in layers {
            for r in l.rect {
                info.extend(r.to_be_bytes());
            }
            info.extend((l.channels.len() as u16).to_be_bytes());
            for (id, data) in &l.channels {
                info.extend(id.to_be_bytes());
                info.extend(((2 + data.len()) as u32).to_be_bytes());
            }
            info.extend_from_slice(b"8BIMnorm");
            info.extend([255u8, 0, 0x08, 0]);
            let mut extra = 0u32.to_be_bytes().to_vec();
            extra.extend(0u32.to_be_bytes());
            let mut pascal = vec![l.name.len() as u8];
            pascal.extend_from_slice(l.name.as_bytes());
            while pascal.len() % 4 != 0 {
                pascal.push(0);
            }
            extra.extend(pascal);
            info.extend((extra.len() as u32).to_be_bytes());
            info.extend(extra);
        }
        for l in layers {
            for (_, data) in &l.channels {
                info.extend(0u16.to_be_bytes());
                info.extend_from_slice(data);
            }
        }
        if info.len() % 2 == 1 {
            info.push(0);
        }
        section.extend((info.len() as u32).to_be_bytes());
        section.extend(info);
        section.extend(0u32.to_be_bytes()); // global layer mask info
    }
    v.extend((section.len() as u32).to_be_bytes());
    v.extend(section);
    v.extend(0u16.to_be_bytes());
    v.extend_from_slice(merged);
    v
}

fn two_layer_psd() -> Vec<u8> {
    let layers = [
        L { name: "base", rect: [0, 0, 2, 2], channels: vec![(0, vec![10; 4]), (1, vec![20; 4]), (2, vec![30; 4])] },
        L {
            name: "top",
            rect: [0, 1, 2, 2],
            channels: vec![(-1, vec![128, 255]), (0, vec![200, 201]), (1, vec![100, 101]), (2, vec![0, 1])],
        },
    ];
    psd(2, 2, &layers, &[10; 12])
}

fn harness() -> Harness {
    Harness::new(Selection { providers: Vec::new(), references: References::TrustAll }).unwrap()
}

#[test]
fn psd_converts_to_a_native_package_that_opens_in_a_session_with_receipt_and_original() {
    let bytes = two_layer_psd();
    let cancel = CancellationToken::default();
    let resolver = TrustAllResolver;
    let (package, imported) = convert_to_native(&bytes, &resolver, &cancel).expect("converted");
    assert_eq!(imported.format, SourceFormat::Psd);
    let proposal_layers = imported.snapshot.document().layers.len();
    assert_eq!(proposal_layers, 2);
    assert_eq!(imported.receipt["layers"], 2);
    assert!(imported.receipt["loss"].as_array().unwrap().iter().any(|e| e["code"] == "profile_assumed_srgb"));

    // The container carries the typed import entries: receipt and original (no records here).
    let opened = read_package(&package, &Limits::default(), folio::Budget::default(), &resolver, &cancel).expect("package reads");
    assert_eq!(opened.import(ImportRole::Original).expect("original").bytes, bytes, "original byte-identical");
    let receipt: Value = serde_json::from_slice(&opened.import(ImportRole::Receipt).expect("receipt").bytes).unwrap();
    assert_eq!(receipt, imported.receipt);
    assert!(opened.import(ImportRole::Records).is_none());
    assert!(opened.loss.is_empty(), "import entries add no loss note");
    assert_eq!(opened.imports.len(), 2);
    assert_eq!(opened.assets.len(), 2, "one tile per layer, digest-keyed");

    // Deterministic: the same source converts to the same bytes.
    assert_eq!(convert_to_native(&bytes, &resolver, &cancel).unwrap().0, package);

    // Open directly through the harness: the session holds the same layer count; save/reopen/verify.
    let h = harness();
    let mut run = h.run(default_actor(), &cancel);
    assert!(run.open_import(&bytes));
    assert!(run.summary());
    let Some(checkpoint) = run.checkpoint() else {
        panic!("checkpoint refused: {}", run.finish("import"));
    };
    assert!(run.reopen_bytes(&checkpoint));
    assert!(run.verify_reopened());
    let report = run.finish("import");
    assert_eq!(report["verdict"], "pass", "{report}");
    let import_op = &report["ops"][0];
    assert_eq!(import_op["op"], "import");
    assert_eq!(import_op["observe"]["frames"], 1);
    assert_eq!(import_op["detail"]["import"]["entries"], serde_json::json!(["receipt", "original"]));
    assert_eq!(report["ops"][1]["detail"]["layers"].as_array().unwrap().len(), proposal_layers);
    // The reopened checkpoint still carries the import entries (the session preserves them verbatim).
    let again = read_package(&checkpoint, &Limits::default(), folio::Budget::default(), &resolver, &cancel).unwrap();
    assert_eq!(again.import(ImportRole::Original).unwrap().bytes, bytes);

    // A layerless PSD (merged image only) converts to one raster layer.
    let layerless = psd(2, 1, &[], &[1, 2, 3, 4, 5, 6]);
    let (_, one) = convert_to_native(&layerless, &resolver, &cancel).expect("layerless converted");
    assert_eq!(one.snapshot.document().layers.len(), 1);
    assert_eq!(one.snapshot.document().layers[0].kind, "raster");
}

#[test]
fn unknown_and_not_yet_importable_formats_are_typed_refusals() {
    let cancel = CancellationToken::default();
    let r = TrustAllResolver;
    let code = |bytes: &[u8]| import(bytes, &r, &cancel).err().map(|e| (e.code(), e.detail()));
    assert_eq!(code(b"GIF89a....").unwrap().0, "format_unrecognised");
    assert_eq!(code(b"").unwrap().0, "format_unrecognised");
    for (bytes, format) in [
        (&b"%PDF-1.7\n%..."[..], "pdf"),
        (&b"II*\0\x08\0\0\0\0\0"[..], "tiff"),
        (&b"MM\0*\0\0\0\x08"[..], "tiff"),
        (&b"II*\0\x10\0\0\0CR\x02\0"[..], "camera_raw"),
        (&b"\0\0\0\x18ftypcrx \0\0\0\x01"[..], "camera_raw"),
        (&b"FUJIFILMCCD-RAW 0201"[..], "camera_raw"),
    ] {
        let (code, detail) = code(bytes).expect("refused, never a fake success");
        assert_eq!(detail["format"], format, "{code}");
        let reaches_importer = match format {
            // With import-pdf a stub PDF reaches press, whose typed refusal passes through.
            "pdf" => cfg!(feature = "import-pdf"),
            "camera_raw" => cfg!(feature = "import-raw"),
            "tiff" => cfg!(feature = "import-raw") || cfg!(feature = "import-tiff"),
            _ => false,
        };
        if reaches_importer {
            // Camera raw and TIFF-family stubs reach develop's container reader or interop-tiff: their
            // typed refusal passes through (a TIFF develop does not claim without import-tiff stays
            // format_unavailable).
            assert!(code == "format_unavailable" || detail["importer_code"] == code, "{format}: {code}");
        } else {
            assert_eq!(code, "format_unavailable", "{format}");
            assert_eq!(detail["reason"], "feature_not_enabled", "{format}");
        }
    }
    assert_eq!(sniff(b"8BPS\0\x02"), Some(SourceFormat::Psb));
    assert_eq!(sniff(b"8BPS\0\x03"), None);
    // A recognised PSD the importer refuses keeps the importer's own code.
    let truncated = &two_layer_psd()[..40];
    match import(truncated, &r, &cancel) {
        Err(ImportError::Importer { format: SourceFormat::Psd, code }) => assert!(!code.is_empty()),
        other => panic!("expected the importer's typed refusal, got {:?}", other.err()),
    }
    // Canceled before anything runs.
    let canceled = CancellationToken::default();
    canceled.cancel();
    assert_eq!(import(&two_layer_psd(), &r, &canceled).err(), Some(ImportError::Canceled));
    // Through the harness: a refusal is one unaddressed record with the typed code and detail.
    let h = harness();
    let mut run = h.run(default_actor(), &cancel);
    assert!(!run.open_import(b"%PDF-1.4"));
    let report = run.finish("import");
    assert_eq!(report["verdict"], "fail");
    if cfg!(feature = "import-pdf") {
        // A truncated PDF is press's typed refusal, passed through unchanged.
        assert_eq!(report["ops"][0]["detail"]["import"]["format"], "pdf", "{report}");
        assert_eq!(report["ops"][0]["detail"]["import"]["importer_code"], report["ops"][0]["error"], "{report}");
    } else {
        assert_eq!(report["ops"][0]["error"], "format_unavailable");
        assert_eq!(report["ops"][0]["detail"]["import"]["feature"], "import-pdf");
    }
}

/// Test scratch directory under TMP, removed on drop (TMP is on D: through studio-env.sh).
struct Scratch(PathBuf);
impl Scratch {
    fn new(label: &str) -> Self {
        let base = std::env::temp_dir();
        assert!(!(cfg!(windows) && base.to_string_lossy().to_ascii_uppercase().starts_with("C:")), "TMP is on C:");
        let dir = base.join(format!("hsk-harness-import-{}-{label}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn cli_json(args: &[&str]) -> (i32, Value) {
    let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    let out = cli::run(&args);
    (out.exit, serde_json::from_str(&out.stdout).unwrap_or(Value::Null))
}

#[test]
fn cli_import_saves_a_native_package_and_never_clobbers_the_source() {
    let scratch = Scratch::new("cli");
    let source = scratch.0.join("in.psd");
    std::fs::write(&source, two_layer_psd()).unwrap();
    let saved = scratch.0.join("out.handshake");
    let (s, o) = (source.to_str().unwrap(), saved.to_str().unwrap());
    let (exit, report) = cli_json(&["import", s, "--references", "trust_all", "--save", o]);
    assert_eq!(exit, 0, "{report}");
    let ops: Vec<&str> = report["ops"].as_array().unwrap().iter().map(|op| op["op"].as_str().unwrap()).collect();
    assert_eq!(ops, ["import", "summary", "save", "reopen", "verify"]);
    let written = std::fs::read(&saved).unwrap();
    let opened = read_package(&written, &Limits::default(), folio::Budget::default(), &TrustAllResolver, &CancellationToken::default())
        .unwrap();
    assert_eq!(opened.import(ImportRole::Original).unwrap().bytes, std::fs::read(&source).unwrap());
    // --save naming the source is a usage error; the source is untouched.
    let before = std::fs::read(&source).unwrap();
    let (exit, _) = cli_json(&["import", s, "--save", s]);
    assert_eq!(exit, 2);
    assert_eq!(std::fs::read(&source).unwrap(), before);
    // Unknown bytes: exit 1 with the typed code.
    let junk = scratch.0.join("junk.bin");
    std::fs::write(&junk, b"not an image").unwrap();
    let (exit, report) = cli_json(&["import", junk.to_str().unwrap()]);
    assert_eq!((exit, report["ops"][0]["error"].as_str()), (1, Some("format_unrecognised")));
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

// ---- psd-tools corpus sweep (external test DATA; ignored by default) --------------------------

/// The one corpus-root convention (`.config/oracle-corpus.toml`): walk up from the workspace root.
fn corpus_root() -> Option<PathBuf> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let config = std::fs::read_to_string(workspace.join(".config/oracle-corpus.toml")).ok()?;
    let value = |key: &str| {
        config.lines().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
        })
    };
    let search = value("search")?;
    let max: usize = value("max-ancestors")?.parse().ok()?;
    let mut at = workspace.canonicalize().ok()?;
    for _ in 0..=max {
        let candidate = at.join(&search);
        if candidate.is_dir() {
            return Some(candidate);
        }
        at = at.parent()?.to_path_buf();
    }
    None
}

/// Files under `dir`, at most `depth` directory levels deep.
fn walk(dir: &Path, depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            if depth > 0 {
                out.extend(walk(&path, depth - 1));
            }
        } else {
            out.push(path);
        }
    }
    out
}

/// Sweep weight of one corpus file: its length plus the composite size its PSD/PSB header declares
/// (channels x height x width x depth / 8; 0 when the header is short or not `8BPS`). Large CMYK or
/// 16-bit documents dominate the import time, so the weight follows the decoded size, not the file order.
fn sweep_weight(path: &Path) -> u64 {
    use std::io::Read;
    let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut header = [0u8; 26];
    let read = std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut header));
    if read.is_err() || &header[..4] != b"8BPS" {
        return len;
    }
    let channels = u64::from(u16::from_be_bytes([header[12], header[13]]));
    let height = u64::from(u32::from_be_bytes([header[14], header[15], header[16], header[17]]));
    let width = u64::from(u32::from_be_bytes([header[18], header[19], header[20], header[21]]));
    let depth = u64::from(u16::from_be_bytes([header[22], header[23]]));
    let decoded = channels.saturating_mul(height).saturating_mul(width).saturating_mul(depth) / 8;
    len.saturating_add(decoded)
}

/// Deterministic size-balanced partition (longest processing time first): files ordered by weight
/// (heaviest first, ties by index) each go to the currently lightest shard (lowest shard on ties).
/// Every index lands in exactly one shard, so the union of all shards is the whole corpus.
fn partition(weights: &[u64], shards: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..weights.len()).collect();
    order.sort_by(|&a, &b| weights[b].cmp(&weights[a]).then(a.cmp(&b)));
    let mut load = vec![0u64; shards];
    let mut assigned = vec![0usize; weights.len()];
    for i in order {
        let (lightest, _) = load.iter().enumerate().min_by(|(a, x), (b, y)| x.cmp(y).then(a.cmp(b))).unwrap();
        load[lightest] = load[lightest].saturating_add(weights[i]);
        assigned[i] = lightest;
    }
    assigned
}

#[test]
fn sweep_partition_covers_every_file_once_and_balances_by_weight() {
    // One heavy file and many light ones: the heavy file gets a shard of its own and every index is
    // assigned exactly once (same coverage as an unsharded sweep).
    let mut weights = vec![1_000u64];
    weights.extend((0..47u64).map(|i| 10 + i));
    let assigned = partition(&weights, SHARDS);
    assert_eq!(assigned.len(), weights.len());
    assert!(assigned.iter().all(|&s| s < SHARDS));
    let heavy = assigned[0];
    assert!(assigned[1..].iter().all(|&s| s != heavy), "the heaviest file shares no shard here: {assigned:?}");
    let mut load = vec![0u64; SHARDS];
    for (i, &s) in assigned.iter().enumerate() {
        load[s] += weights[i];
    }
    let light: Vec<u64> = load.iter().enumerate().filter(|(s, _)| *s != heavy).map(|(_, l)| *l).collect();
    let (min, max) = (light.iter().min().unwrap(), light.iter().max().unwrap());
    assert!(max - min <= 56, "light shards balanced within one file weight: {load:?}");
    // Deterministic: same input, same assignment.
    assert_eq!(partition(&weights, SHARDS), assigned);
}

/// Bounded sweep over every PSD/PSB in `oracle-corpus/psd-tools/psd_files`: each file goes through
/// `convert_to_native` (sniff, interop-psd, native package) under `catch_unwind`; prints ok /
/// typed-refusal (by code) / panic counts. Panics are defects. The corpus is split into SHARDS
/// size-balanced shards ([`partition`] over [`sweep_weight`]) so each test stays under the runner's
/// 120 s per-test cap; the union of the shards is every file (same coverage as before re-sharding).
fn sweep(shard: usize) {
    let Some(root) = corpus_root() else {
        println!("NOT_PROVEN: psd_tools_sweep shard={shard} reason=corpus_root_absent");
        return;
    };
    let dir = root.join("psd-tools/psd_files");
    let Ok(read) = std::fs::read_dir(&dir) else {
        println!("NOT_PROVEN: psd_tools_sweep shard={shard} reason=psd_files_absent");
        return;
    };
    drop(read);
    let mut files: Vec<PathBuf> = walk(&dir, 4)
        .into_iter()
        .filter(|p| p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("psd") || e.eq_ignore_ascii_case("psb")))
        .collect();
    files.sort();
    let weights: Vec<u64> = files.iter().map(|p| sweep_weight(p)).collect();
    let assigned = partition(&weights, SHARDS);
    let started = std::time::Instant::now();
    let cancel = CancellationToken::default();
    let (mut ok, mut total, mut weight, mut panics) = (0usize, 0usize, 0u64, Vec::new());
    let mut refusals: std::collections::BTreeMap<String, usize> = Default::default();
    for (i, path) in files.iter().enumerate() {
        if assigned[i] != shard {
            continue;
        }
        total += 1;
        weight += weights[i];
        let bytes = std::fs::read(path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| convert_to_native(&bytes, &TrustAllResolver, &cancel).map(|(p, d)| (p.len(), d.snapshot.document().layers.len()))));
        match outcome {
            Ok(Ok(_)) => ok += 1,
            Ok(Err(e)) => *refusals.entry(e.code().to_string()).or_default() += 1,
            Err(_) => panics.push(name),
        }
    }
    println!(
        "SWEEP psd_tools shard={shard}/{SHARDS} corpus_files={} files={total} weight={weight} elapsed_ms={} ok={ok} refused={} panics={} refusals={refusals:?} panic_files={panics:?}",
        files.len(),
        started.elapsed().as_millis(),
        refusals.values().sum::<usize>(),
        panics.len()
    );
    assert!(panics.is_empty(), "panics are defects: {panics:?}");
}

const SHARDS: usize = 16;

macro_rules! sweep_shards {
    ($($name:ident = $shard:expr;)*) => {$(
        #[test]
        #[ignore = "external corpus sweep (psd-tools fixtures under the oracle corpus); run with --run-ignored ignored-only"]
        fn $name() {
            sweep($shard);
        }
    )*};
}

sweep_shards! {
    psd_tools_sweep_shard_0 = 0;
    psd_tools_sweep_shard_1 = 1;
    psd_tools_sweep_shard_2 = 2;
    psd_tools_sweep_shard_3 = 3;
    psd_tools_sweep_shard_4 = 4;
    psd_tools_sweep_shard_5 = 5;
    psd_tools_sweep_shard_6 = 6;
    psd_tools_sweep_shard_7 = 7;
    psd_tools_sweep_shard_8 = 8;
    psd_tools_sweep_shard_9 = 9;
    psd_tools_sweep_shard_10 = 10;
    psd_tools_sweep_shard_11 = 11;
    psd_tools_sweep_shard_12 = 12;
    psd_tools_sweep_shard_13 = 13;
    psd_tools_sweep_shard_14 = 14;
    psd_tools_sweep_shard_15 = 15;
}

#[test]
fn sweep_shard_count_matches_the_declared_shard_tests() {
    // A shard without a test would silently drop files: the macro list above must name 0..SHARDS.
    let src = include_str!("import_psd.rs");
    let declared = (0..SHARDS).filter(|s| src.contains(&format!("psd_tools_sweep_shard_{s} = {s};"))).count();
    assert_eq!(declared, SHARDS);
    assert!(!src.contains(&format!("psd_tools_sweep_shard_{SHARDS} = ")));
}

// ---- camera raw through develop (feature import-raw; external CC0 samples, ignored by default) --

/// A corpus file pinned by SHA-256, or a printed NOT_PROVEN reason.
#[cfg(feature = "import-raw")]
fn pinned(test: &str, relative: &str, sha256: &str) -> Option<Vec<u8>> {
    let Some(root) = corpus_root() else {
        println!("NOT_PROVEN: {test} reason=corpus_root_absent");
        return None;
    };
    let Ok(bytes) = std::fs::read(root.join(relative)) else {
        println!("NOT_PROVEN: {test} reason=file_absent {relative}");
        return None;
    };
    let digest = hsk_studio_package::sha256_hex_of(&bytes);
    if digest != sha256 {
        println!("NOT_PROVEN: {test} reason=sha256_mismatch {relative} {digest}");
        return None;
    }
    Some(bytes)
}

#[cfg(feature = "import-raw")]
#[test]
#[ignore = "external CC0 DNG sample (oracle corpus dng/); run with --run-ignored ignored-only"]
fn dng_converts_to_one_developed_raster_layer_and_opens_in_a_session() {
    // The smallest corpus DNG (1920x818, lossless JPEG), so the debug build stays inside the per-test cap.
    let Some(bytes) = pinned(
        "dng_converts",
        "dng/canon-eos-5d3-mlv-14bit-lossless.dng",
        "1d77dcc6cdb839b10c1948f0b4471984ab48cf15e973fb77520584cee72b603e",
    ) else {
        return;
    };
    assert_eq!(sniff(&bytes), Some(SourceFormat::Tiff));
    let cancel = CancellationToken::default();
    let h = harness();
    let mut run = h.run(default_actor(), &cancel);
    assert!(run.open_import(&bytes), "DNG imported");
    assert!(run.summary());
    let Some(checkpoint) = run.checkpoint() else {
        panic!("checkpoint refused: {}", run.finish("import"));
    };
    let report = run.finish("import");
    assert_eq!(report["verdict"], "pass", "{}", report["ops"][0]);
    let import_op = &report["ops"][0]["detail"]["import"];
    assert_eq!(import_op["format"], "tiff");
    assert_eq!(import_op["layers"], 1);
    let layers = report["ops"][1]["detail"]["layers"].as_array().unwrap();
    assert_eq!((layers.len(), layers[0]["kind"].as_str()), (1, Some("raster")));
    // The package carries the original DNG and the receipt with the develop route and loss entries.
    let opened = read_package(&checkpoint, &Limits::default(), folio::Budget::default(), &TrustAllResolver, &cancel).unwrap();
    assert_eq!(opened.import_bytes(ImportRole::Original), Some(&bytes[..]));
    let receipt: Value = serde_json::from_slice(opened.import_bytes(ImportRole::Receipt).unwrap()).unwrap();
    assert_eq!(receipt["source_format"], "dng");
    assert!(receipt["develop"]["route"].is_string());
    assert!(receipt["loss"].as_array().unwrap().iter().any(|e| e["code"] == "float32_assumed_linear_light"));
    println!("DNG window={} tiles={} package_bytes={}", receipt["develop"]["window"], receipt["tiles"], checkpoint.len());
}

#[cfg(feature = "import-raw")]
#[test]
#[ignore = "external CC0 camera raw samples (oracle corpus raw-vendor/); run with --run-ignored ignored-only"]
fn proprietary_raw_refusals_pass_through_develop_unchanged() {
    // The import refusal is develop's own first refusal on the import path, unchanged. CR3 decodes
    // since SMB-DEVELOP-RAW-COMPRESSED (ec53c53) but the decoded container carries no colour tags, so
    // develop's colour solve refuses missing_colour_data; the SuperCCD RAF has no TIFF raw block, so
    // decode_container refuses decoder_unavailable. A silent import of either is a failure.
    let cancel = CancellationToken::default();
    let cases = [
        ("raw-vendor/canon-eos-r6.cr3", "solve_colour", "missing_colour_data"),
        ("raw-vendor/fujifilm-finepix-s5000.raf", "decode_container", "decoder_unavailable"),
    ];
    for (file, step, code) in cases {
        let Some(root) = corpus_root() else {
            println!("NOT_PROVEN: raw_refusals reason=corpus_root_absent");
            return;
        };
        let Ok(bytes) = std::fs::read(root.join(file)) else {
            println!("NOT_PROVEN: raw_refusals reason=file_absent {file}");
            continue;
        };
        let direct = match hsk_studio_develop::decode_container(&bytes) {
            Err(e) => ("decode_container", e.code()),
            Ok(raw) => ("solve_colour", raw.solve_colour().map(|_| "solved").unwrap_or_else(|e| e.code())),
        };
        assert_eq!(direct, (step, code), "{file}: develop's own first refusal");
        match import(&bytes, &TrustAllResolver, &cancel) {
            Err(ImportError::Importer { format: SourceFormat::CameraRaw, code: got }) => assert_eq!(got, code, "{file}"),
            Err(other) => panic!("{file}: unexpected refusal {other}"),
            Ok(_) => panic!("{file}: imported, expected develop's {step} refusal {code}"),
        }
    }
}
