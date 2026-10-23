//! Provenance hashes must describe canonical committed bytes, never a platform working tree.
//! Vendored sources are text (root .gitattributes `text=auto eol=lf`): their git blobs are LF, so a
//! CRLF checkout is normalized to LF before hashing. Fixtures are `-text`: exact bytes, no normalization.
use sha2::{Digest, Sha256};
use std::path::Path;

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}
fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}
fn lf(bytes: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
            i += 1;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}
fn json(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn vendored_derivative_hashes_match_committed_bytes() {
    let mut checked = 0;
    let mut drift = Vec::new();
    for (provenance, base) in [
        ("third-party/rustybuzz-0.20.1/source-provenance.json", "src/provider/rustybuzz"),
        ("third-party/ttf-parser-0.25.1/SOURCE_PROVENANCE.json", ""),
        ("third-party/unicode-bidi-0.3.18/provenance.json", ""),
    ] {
        for entry in json(&manifest().join(provenance))["derivative_files"].as_array().unwrap() {
            let path = entry["path"].as_str().unwrap();
            let bytes = lf(std::fs::read(manifest().join(base).join(path)).unwrap());
            let sha = hex(&bytes);
            let length_ok = entry
                .get("bytes")
                .is_none_or(|b| b.as_u64() == Some(bytes.len() as u64));
            if entry["sha256"] != sha.as_str() || !length_ok {
                drift.push(format!("{provenance}: {path} recorded {} actual {sha}", entry["sha256"]));
            }
            checked += 1;
        }
    }
    println!("vendored provenance: checked {checked}, drift {}", drift.len());
    assert_eq!(checked, 150, "74 rustybuzz + 67 ttf-parser + 9 unicode-bidi derivative files");
    assert!(drift.is_empty(), "{drift:#?}");
}

#[test]
fn fixture_hashes_match_committed_bytes() {
    let fixtures = manifest().join("tests/fixtures");
    let mut checked = 0;
    let mut drift = Vec::new();
    for entry in json(&fixtures.join("FIXTURES_PROVENANCE.json"))["files"].as_array().unwrap() {
        let file = entry["file"].as_str().unwrap();
        let bytes = std::fs::read(fixtures.join(file)).unwrap();
        let sha = hex(&bytes);
        if entry["sha256"] != sha.as_str() || entry["bytes"].as_u64() != Some(bytes.len() as u64) {
            drift.push(format!("{file}: recorded {} actual {sha}", entry["sha256"]));
        }
        checked += 1;
    }
    println!("fixture provenance: checked {checked}, drift {}", drift.len());
    assert!(checked > 0);
    assert!(drift.is_empty(), "{drift:#?}");
}
