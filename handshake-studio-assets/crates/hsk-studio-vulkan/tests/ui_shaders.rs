//! SMB-VK-UI-PASS-1 item 2, no device and no naga: the checked-in SPIR-V matches its manifest (sha256 of ui.wgsl and
//! of every .spv, recomputed here with a dependency-free SHA-256), every module has a valid SPIR-V 1.6 header, the
//! spv-build tool keeps its own workspace and a Cargo.lock that is a strict subset of the root Cargo.lock, and naga is
//! in no product manifest (O3, D7).
mod support;

use std::path::{Path, PathBuf};

const REGENERATE: &str = "cargo run --offline --locked --manifest-path crates/hsk-studio-vulkan/spv-build/Cargo.toml --target-dir \"$CARGO_TARGET_DIR/spv-build\"";

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(p: &Path) -> Vec<u8> {
    std::fs::read(p).unwrap_or_else(|e| panic!("{}: {e}; regenerate with: {REGENERATE}", p.display()))
}

/// The string value of `"key":"value"` occurrences, in order (the manifest is flat JSON written by spv-build).
fn values<'a>(json: &'a str, key: &str) -> Vec<&'a str> {
    let pat = format!("\"{key}\":\"");
    let mut out = Vec::new();
    let mut rest = json;
    while let Some(i) = rest.find(&pat) {
        let tail = &rest[i + pat.len()..];
        let end = tail.find('"').expect("closing quote");
        out.push(&tail[..end]);
        rest = &tail[end..];
    }
    out
}

#[test]
fn manifest_hashes_match_the_wgsl_source_and_every_spirv_module() {
    let dir = crate_dir().join("shaders");
    let manifest = String::from_utf8(read(&dir.join("manifest.json"))).unwrap();
    assert!(manifest.contains("\"naga\":\"27.0.3\""), "{manifest}");
    assert!(manifest.contains("\"source\":\"shaders/ui.wgsl\""), "{manifest}");
    let source_sha = values(&manifest, "source_sha256");
    assert_eq!(source_sha.len(), 1);
    let wgsl = read(&dir.join("ui.wgsl"));
    assert_eq!(support::sha256_hex(&wgsl), source_sha[0], "ui.wgsl changed without regeneration; run: {REGENERATE}");
    let spvs = values(&manifest, "spv");
    let shas = values(&manifest, "sha256");
    assert_eq!(spvs.len(), 2, "vertex and fragment entry points: {manifest}");
    assert_eq!(shas.len(), spvs.len());
    for (spv, sha) in spvs.iter().zip(&shas) {
        let bytes = read(&crate_dir().join(spv));
        assert_eq!(&support::sha256_hex(&bytes), sha, "{spv} differs from the manifest; run: {REGENERATE}");
    }
    for key in ["lang_version", "flags", "bounds_check_policies", "debug_info", "capabilities", "options_sha256", "regenerate"] {
        assert!(manifest.contains(&format!("\"{key}\"")), "manifest lacks {key}");
    }
}

#[test]
fn every_spirv_module_has_a_valid_1_6_header() {
    let dir = crate_dir().join("shaders");
    let mut n = 0;
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|x| x.to_str()) != Some("spv") {
            continue;
        }
        n += 1;
        let b = read(&p);
        assert!(b.len() >= 20 && b.len().is_multiple_of(4), "{}: {} bytes", p.display(), b.len());
        let w = |i: usize| u32::from_le_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]);
        assert_eq!(w(0), 0x0723_0203, "{}: magic", p.display());
        assert_eq!(w(1), 0x0001_0600, "{}: SPIR-V 1.6", p.display());
        assert!(w(3) > 0, "{}: id bound", p.display());
    }
    assert_eq!(n, 2, "ui.vs_main.spv and ui.fs_main.spv");
}

/// `(name, version, checksum)` of every `[[package]]` block of a Cargo.lock.
fn packages(lock: &str) -> Vec<(String, String, Option<String>)> {
    let mut out = Vec::new();
    for block in lock.split("[[package]]").skip(1) {
        let field = |k: &str| {
            block.lines().find_map(|l| l.strip_prefix(&format!("{k} = \"")).and_then(|v| v.strip_suffix('"')).map(str::to_owned))
        };
        out.push((field("name").expect("name"), field("version").expect("version"), field("checksum")));
    }
    out
}

#[test]
fn spv_build_lock_is_a_strict_subset_of_the_root_lock_and_naga_stays_out_of_products() {
    let tool = crate_dir().join("spv-build");
    let manifest = String::from_utf8(read(&tool.join("Cargo.toml"))).unwrap();
    assert!(manifest.lines().any(|l| l.trim() == "[workspace]"), "spv-build has its own workspace (no root member)");
    assert!(manifest.contains("naga = { version = \"=27.0.3\", default-features = false, features = [\"wgsl-in\", \"spv-out\"] }"), "{manifest}");
    let root = String::from_utf8(read(&crate_dir().join("../../Cargo.lock"))).unwrap();
    let root_pkgs = packages(&root);
    let tool_pkgs = packages(&String::from_utf8(read(&tool.join("Cargo.lock"))).unwrap());
    assert!(tool_pkgs.len() > 1);
    for p in &tool_pkgs {
        if p.0 == "spv-build" {
            continue;
        }
        assert!(p.2.is_some(), "{p:?}: a registry package");
        assert!(root_pkgs.contains(p), "{p:?} is not in the root Cargo.lock (name, version and checksum)");
    }
    assert!(!tool_pkgs.iter().any(|p| p.0 == "petgraph"), "no spv-in");
    let root_toml = String::from_utf8(read(&crate_dir().join("../../Cargo.toml"))).unwrap();
    assert!(!root_toml.contains("hsk-studio-vulkan/spv-build"), "spv-build is no root member");
    let own = String::from_utf8(read(&crate_dir().join("Cargo.toml"))).unwrap();
    for banned in ["naga", "wgpu", "egui", "eframe", "winit"] {
        assert!(!own.contains(banned), "hsk-studio-vulkan's manifest names {banned}");
    }
}
