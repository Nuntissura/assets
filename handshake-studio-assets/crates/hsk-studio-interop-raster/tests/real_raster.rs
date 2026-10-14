//! Corpus test over PngSuite 2017jul19 and four libjpeg-turbo 3.0.4 test images
//! (`oracle-corpus/raster-common/provenance.json`, which also carries Pillow 12.2.0's decode of each file as
//! the independent oracle). `#[ignore]`d: the corpus is external test DATA, never committed. Run with
//! `cargo nextest run -p hsk-studio-interop-raster --test real_raster --run-ignored ignored-only --no-capture`.
//!
//! The corpus root comes from the checked-in `.config/oracle-corpus.toml` (V4-03): start at the workspace
//! root, walk up at most `max-ancestors` parents, first ancestor holding `<search>` as a directory.
use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_raster::*;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn corpus_root() -> Option<PathBuf> {
    let ws = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cfg = std::fs::read_to_string(ws.join(".config/oracle-corpus.toml")).ok()?;
    let value = |key: &str| {
        cfg.lines().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == key).then(|| v.trim().trim_matches('"').to_owned())
        })
    };
    let search = value("search")?;
    let max: usize = value("max-ancestors")?.parse().ok()?;
    let ws = ws.canonicalize().ok()?;
    ws.ancestors().skip(1).take(max).map(|a| a.join(&search)).find(|p| p.is_dir())
}

struct Entry {
    file: String,
    sha256: String,
    /// Pillow oracle: (kind, sha256) for PNG, channel means for JPEG.
    oracle_kind: Option<String>,
    oracle_sha: Option<String>,
    means: Vec<f64>,
}

fn string_after(text: &str, key: &str) -> Option<String> {
    let i = text.find(key)?;
    Some(text[i + key.len()..].chars().take_while(|c| *c != '"').collect())
}

/// Flat scan of provenance.json (one object per corpus file, keys as written by the download script).
fn provenance(text: &str) -> Vec<Entry> {
    let starts: Vec<usize> = text.match_indices("\"file\": \"").map(|(i, _)| i).collect();
    let mut out = Vec::new();
    for (n, &s) in starts.iter().enumerate() {
        let chunk = &text[s..starts.get(n + 1).copied().unwrap_or(text.len())];
        let file = string_after(chunk, "\"file\": \"").unwrap_or_default();
        if !(file.starts_with("pngsuite/") || file.starts_with("libjpeg-turbo/")) {
            continue;
        }
        let sha256 = string_after(chunk, "\"sha256\": \"").unwrap_or_default();
        let oracle = chunk.find("\"pillow_oracle\"").map(|i| &chunk[i..]);
        let means = oracle
            .and_then(|o| o.find("\"channel_means\": [").map(|i| &o[i + 18..]))
            .map(|m| m[..m.find(']').unwrap_or(0)].split(',').filter_map(|v| v.trim().parse().ok()).collect())
            .unwrap_or_default();
        out.push(Entry {
            file,
            sha256,
            oracle_kind: oracle.and_then(|o| string_after(o, "\"kind\": \"")),
            oracle_sha: oracle.and_then(|o| string_after(o, "\"sha256\": \"")),
            means,
        });
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Our samples in Pillow's `convert("RGBA")` layout (16-bit: high byte, as Pillow's 16-bit unpackers).
fn rgba8(img: &RasterImage) -> Vec<u8> {
    let v: Vec<u8> = match &img.samples {
        Samples::U8(v) => v.clone(),
        Samples::U16(v) => v.iter().map(|s| (s >> 8) as u8).collect(),
    };
    let mut out = Vec::with_capacity(v.len() * 4);
    for px in v.chunks_exact(img.colour.channels()) {
        let rgba = match img.colour {
            Colour::Gray => [px[0], px[0], px[0], 255],
            Colour::GrayA => [px[0], px[0], px[0], px[1]],
            Colour::Rgb => [px[0], px[1], px[2], 255],
            Colour::Rgba => [px[0], px[1], px[2], px[3]],
            Colour::Cmyk => panic!("no CMYK PNG"),
        };
        out.extend_from_slice(&rgba);
    }
    out
}

#[test]
#[ignore = "needs the downloaded PngSuite/libjpeg-turbo corpus under the oracle-corpus root"]
fn corpus_files_decode_as_pillow_or_refuse() {
    let Some(root) = corpus_root() else {
        println!("NOT_PROVEN: real_raster corpus reason=oracle-corpus root not found via .config/oracle-corpus.toml");
        return;
    };
    let dir = root.join("raster-common");
    let Ok(text) = std::fs::read_to_string(dir.join("provenance.json")) else {
        println!("NOT_PROVEN: real_raster corpus reason=raster-common/provenance.json absent");
        return;
    };
    let entries = provenance(&text);
    assert_eq!(entries.len(), 180, "176 PngSuite PNGs + 4 libjpeg-turbo JPEGs");
    let lim = Limits::default();
    let live = CancellationToken::default();
    let mut mismatches = Vec::new();
    let mut decoded = std::collections::BTreeMap::new();
    for e in &entries {
        let bytes = std::fs::read(dir.join(&e.file)).expect("corpus file listed in provenance");
        assert_eq!(hex(&Sha256::digest(&bytes)), e.sha256, "sha256 of {}", e.file);
        let name = e.file.rsplit('/').next().unwrap_or_default();
        let result = decode(&bytes, &lim, &live);
        if let Some(png) = name.strip_suffix(".png") {
            if png.starts_with('x') {
                // PngSuite x-files are corrupt by construction: a typed refusal, never a decode.
                let code = result.as_ref().err().map(RasterError::code);
                println!("REFUSED {name} => {code:?}");
                assert!(code.is_some(), "{name} must be refused");
                continue;
            }
            let img = result.unwrap_or_else(|err| panic!("{name}: {err}"));
            let meta = img.png.as_ref().unwrap();
            if meta.source_colour_type == 0 && meta.transparency_chunk {
                // Gray tRNS (PNG spec 11.3.2.1): the key is a sample value at the file bit depth; exactly the
                // pixels equal to it (after sub-byte scaling) are transparent.
                let at = bytes.windows(4).position(|w| w == b"tRNS").unwrap() + 4;
                let key = u32::from(u16::from_be_bytes([bytes[at], bytes[at + 1]]));
                let bd = u32::from(meta.source_bit_depth);
                let scaled = if bd < 8 { key * (255 / ((1 << bd) - 1)) } else { key };
                let px: Vec<(u32, u32)> = match &img.samples {
                    Samples::U8(v) => v.chunks_exact(2).map(|p| (u32::from(p[0]), u32::from(p[1]))).collect(),
                    Samples::U16(v) => v.chunks_exact(2).map(|p| (u32::from(p[0]), u32::from(p[1]))).collect(),
                };
                let max = if img.depth() == 16 { 65_535 } else { 255 };
                assert!(px.iter().all(|&(g, a)| a == if g == scaled { 0 } else { max }), "{name}: tRNS alpha");
            }
            let mut ours = match e.oracle_kind.as_deref() {
                // Pillow I;16 carries the gray channel only.
                Some("gray_u16le") => match &img.samples {
                    Samples::U16(v) => v.iter().step_by(img.colour.channels()).flat_map(|s| s.to_le_bytes()).collect(),
                    Samples::U8(_) => Vec::new(),
                },
                _ => rgba8(&img),
            };
            if name == "tbbn0g04.png" {
                // Pillow 12.2.0 keeps the 4-bit key (15) and compares it with the 8-bit expanded samples, so it
                // marks no pixel transparent; the spec check above proves our alpha, the gray must match Pillow.
                ours.chunks_exact_mut(4).for_each(|p| p[3] = 255);
            }
            if e.oracle_sha.as_deref() != Some(hex(&Sha256::digest(&ours)).as_str()) {
                mismatches.push(name.to_owned());
            }
            decoded.insert(png.to_owned(), img);
        } else {
            match name {
                "testimgari.jpg" => assert_eq!(result.unwrap_err().code(), "arithmetic_coding_unsupported"),
                "testorig12.jpg" => assert_eq!(result.unwrap_err().code(), "jpeg_precision_unsupported"),
                _ => {
                    let img = result.unwrap_or_else(|err| panic!("{name}: {err}"));
                    assert_eq!((img.width, img.height, img.colour), (227, 149, Colour::Rgb), "{name}");
                    let Samples::U8(v) = &img.samples else { panic!("8-bit") };
                    for (c, want) in e.means.iter().enumerate() {
                        let mean = v.iter().skip(c).step_by(3).map(|s| f64::from(*s)).sum::<f64>() / (227.0 * 149.0);
                        println!("JPEG {name} channel {c}: mean {mean:.4} vs Pillow {want:.4}");
                        assert!((mean - want).abs() < 0.5, "{name} channel {c}");
                    }
                }
            }
        }
    }
    println!("PNG decoded {} files; mismatches vs Pillow: {mismatches:?}", decoded.len());
    assert!(mismatches.is_empty(), "samples differ from Pillow: {mismatches:?}");
    // Adam7 and non-interlaced encodings of the same picture decode identically.
    let mut pairs = 0;
    for (name, img) in &decoded {
        if let Some(rest) = name.strip_prefix("basi") {
            assert_eq!(img.samples, decoded[&format!("basn{rest}")].samples, "{name}");
            assert!(img.png.as_ref().unwrap().interlaced);
            pairs += 1;
        }
    }
    assert_eq!(pairs, 15);
}
