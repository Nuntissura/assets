//! Real consumer: write a Folio document (+ digest-named asset files) to a `.handshake`
//! container with atomic save, or read one back under default limits.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{Budget, ContentDigest, Inspection, Unresolved, inspect_bytes};
use hsk_studio_package::{
    AssetError, AssetSource, DESCRIPTOR, Limits, LossReason, read_package, save_atomic,
    write_package,
};
use std::{
    env,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

/// Consumer safety ceiling for whole-file reads (the library itself is limit-driven).
const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;

struct DirAssets(PathBuf);

impl AssetSource for DirAssets {
    fn read(&self, digest: &ContentDigest, max: u64) -> Result<Vec<u8>, AssetError> {
        let hex = &digest.digest;
        if digest.algorithm != "sha256" || hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(AssetError::Absent);
        }
        let mut bytes = Vec::new();
        File::open(self.0.join(hex))
            .map_err(|_| AssetError::Absent)?
            .take(max.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|_| AssetError::Absent)?;
        if bytes.len() as u64 > max {
            return Err(AssetError::TooLarge);
        }
        Ok(bytes)
    }
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|_| "input unavailable".to_owned())?
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "input read failed".to_owned())?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err("input exceeds consumer safety ceiling".to_owned());
    }
    Ok(bytes)
}

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();
    let token = CancellationToken::default();
    let limits = Limits::default();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["--descriptor"] => {
            println!("{DESCRIPTOR}");
            Ok(())
        }
        ["--write", doc, out, rest @ ..] => {
            let assets = match rest {
                [] => PathBuf::new(),
                ["--assets", dir] => PathBuf::from(dir),
                _ => return Err("unknown arguments".into()),
            };
            let bytes = read_bounded(Path::new(doc))?;
            let snapshot = match inspect_bytes(&bytes, Budget::default(), &token, &Unresolved)
                .map_err(|d| format!("document_rejected:{:?}", d.code))?
            {
                Inspection::Editable(s) => s,
                Inspection::ReadOnly(_) => return Err("document is read-only".into()),
            };
            let mut package = Vec::new();
            let receipt = write_package(&snapshot, &DirAssets(assets), &limits, &token, &mut package)
                .map_err(|e| e.code().to_owned())?;
            save_atomic(Path::new(out), &package, &limits, &token).map_err(|e| e.code().to_owned())?;
            println!(
                "{}",
                summary_json(&[
                    ("entries", receipt.entries.to_string()),
                    ("bytes_written", receipt.bytes_written.to_string()),
                    ("doc_revision", receipt.doc_revision.to_string()),
                    ("zip64", receipt.zip64.to_string()),
                ])
            );
            Ok(())
        }
        ["--read", input] => {
            let bytes = read_bounded(Path::new(input))?;
            let opened = read_package(&bytes, &limits, Budget::default(), &Unresolved, &token)
                .map_err(|e| e.code().to_owned())?;
            let loss: Vec<&str> = opened
                .loss
                .iter()
                .map(|n| match n.reason {
                    LossReason::Unlisted => "unlisted",
                    LossReason::Opaque => "opaque",
                    LossReason::Preview => "preview",
                })
                .collect();
            println!(
                "{}",
                summary_json(&[
                    ("document_id", format!("\"{}\"", opened.manifest.document_id)),
                    ("revision", opened.manifest.revision.to_string()),
                    ("editable", opened.snapshot().is_some().to_string()),
                    ("assets", opened.assets.len().to_string()),
                    ("quarantined", opened.quarantined.len().to_string()),
                    ("loss", format!("{loss:?}")),
                ])
            );
            Ok(())
        }
        _ => Err("usage: --descriptor | --write DOC.json OUT.handshake [--assets DIR] | --read IN.handshake".into()),
    }
}

/// Minimal JSON object from pre-rendered values (keys are fixed literals above).
fn summary_json(fields: &[(&str, String)]) -> String {
    let body: Vec<String> = fields.iter().map(|(k, v)| format!("\"{k}\":{v}")).collect();
    format!("{{{}}}", body.join(","))
}

fn main() {
    if let Err(message) = run() {
        eprintln!("{message}");
        std::process::exit(2);
    }
}
