//! Real consumer: write a Folio document (+ digest-named asset files) to a `.handshake`
//! container with atomic save, or read one back under default limits.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{Budget, ContentDigest, Inspection, Unresolved, inspect_bytes};
use hsk_studio_package::{
    AssetError, AssetSource, Codec, DESCRIPTOR, Extras, Limits, LossReason, PixelFormat,
    PreviewImage, PreviewInfo, WriteOptions, read_package_stream, save_atomic, sweep_stale_temps,
    write_package_with,
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
        if digest.algorithm != "sha256"
            || hex.len() != 64
            || !hex.bytes().all(|b| b.is_ascii_hexdigit())
        {
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

/// `--preview PIXELS FORMAT WxH COLOR`: raw tightly packed pixels from a file.
fn read_preview_arg(
    file: &str,
    format: &str,
    size: &str,
    color: &str,
) -> Result<PreviewImage, String> {
    let format = match format {
        "gray8" => PixelFormat::Gray8,
        "rgb8" => PixelFormat::Rgb8,
        "rgba8" => PixelFormat::Rgba8,
        _ => return Err("unknown preview format".into()),
    };
    let (width, height) = size.split_once('x').ok_or("preview size is WxH")?;
    let info = PreviewInfo {
        format,
        width: width.parse().map_err(|_| "invalid preview width")?,
        height: height.parse().map_err(|_| "invalid preview height")?,
        color: color.to_owned(),
    };
    Ok(PreviewImage {
        pixels: read_bounded(Path::new(file))?,
        info,
    })
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
            let mut assets = PathBuf::new();
            let mut options = WriteOptions::default();
            let mut previews: Vec<PreviewImage> = Vec::new();
            let mut flags = rest.iter();
            while let Some(flag) = flags.next() {
                match *flag {
                    "--assets" => assets = PathBuf::from(flags.next().ok_or("missing --assets value")?),
                    "--zstd" => options.asset_codec = Codec::Zstd,
                    "--shuffle" => options.asset_codec = Codec::ShuffleZstd,
                    "--preview" => {
                        let mut next = || flags.next().ok_or("missing --preview value");
                        let (file, format, size, color) = (next()?, next()?, next()?, next()?);
                        previews.push(read_preview_arg(file, format, size, color)?);
                    }
                    _ => return Err("unknown arguments".into()),
                }
            }
            let bytes = read_bounded(Path::new(doc))?;
            let snapshot = match inspect_bytes(&bytes, Budget::default(), &token, &Unresolved)
                .map_err(|d| format!("document_rejected:{:?}", d.code))?
            {
                Inspection::Editable(s) => s,
                Inspection::ReadOnly(_) => return Err("document is read-only".into()),
            };
            let mut package = Vec::new();
            let receipt = write_package_with(
                &snapshot,
                &DirAssets(assets),
                &Extras {
                    quarantined: &[],
                    previews: &previews,
                },
                &options,
                &limits,
                &token,
                &mut package,
            )
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
            let file = File::open(input).map_err(|_| "input unavailable".to_owned())?;
            let opened = read_package_stream(file, &limits, Budget::default(), &Unresolved, &token)
                .map_err(|e| e.code().to_owned())?;
            let loss: Vec<&str> = opened
                .loss
                .iter()
                .map(|n| match n.reason {
                    LossReason::Unlisted => "unlisted",
                    LossReason::Opaque => "opaque",
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
                    ("previews", opened.previews.len().to_string()),
                    ("loss", format!("{loss:?}")),
                ])
            );
            Ok(())
        }
        ["--sweep", target] => {
            let report = sweep_stale_temps(Path::new(target), &token).map_err(|e| e.code().to_owned())?;
            println!(
                "{}",
                summary_json(&[
                    ("removed", format!("{:?}", report.removed)),
                    ("skipped_live", report.skipped_live.to_string()),
                    ("ignored", report.ignored.to_string()),
                ])
            );
            Ok(())
        }
        _ => Err("usage: --descriptor | --write DOC.json OUT.handshake [--assets DIR] [--zstd|--shuffle] [--preview PIXELS FORMAT WxH COLOR] | --read IN.handshake | --sweep TARGET".into()),
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
