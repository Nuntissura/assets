//! Container-consolidation identity record (lane SMB-DEVELOP-TIFF-CONSOLIDATE): digest of the decoded
//! CFA samples and of the developed default window (or the typed outcome) for every DNG and vendor RAW
//! corpus file. `#[ignore]`d (needs the oracle corpus, never committed). It prints one `IDENT` line per
//! file; with `PINNED` filled from the pre-switch HEAD the same test asserts byte identity afterwards.
//! A missing corpus root or file prints `NOT_PROVEN:` and proves nothing.
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_develop::*;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// (corpus subdirectory, file name, expected IDENT value from the pre-switch HEAD).
const PINNED: &[(&str, &str, &str)] = &[
    ("dng", "adobe-dng-converter-canon-eos-5d3-16bit-uncompressed.dng", "5920x3950|16|cfa=d4f894f5e3414125ffa2b8a94637ab44034f9de931a6ca3ef6025c97cbd28a8d|dev=5760x3840:(5133, 2322):7332f8cafcebb9b56f707f8f01e6deca8e9ea8e2bc2d8ddc9709a4b64e981185"),
    ("dng", "canon-eos-5d3-mlv-14bit-lossless.dng", "1920x818|14|cfa=786415ab4fd024a1a066f13b7b900e99b0f2fcb4cfefd462511d2ea98934aa4e|dev=1920x818:(176919, 0):df7fe25d0ac83a989502df40ec85251242fa9af2983070842f46b031d9cabf3b"),
    ("dng", "nikon-d80-16bit-lossless-tiled-linearized.dng", "3900x2611|16|cfa=01504457f9724ba97d9a6795f920148d5ac153a68f48c88dc830ed8556a5e625|dev=3872x2592:(0, 57267):a93a3adf24dea932e9ed6f357b6c1b616ff828c0428725f6d1f991f7043217e1"),
    ("dng", "samsung-sm-g973u-16bit-uncompressed.dng", "decode_err=DecoderUnavailable(\"jpeg\")"),
    ("raw-vendor", "canon-eos-350d-raw.cr2", "3516x2328|12|cfa=29f020448207b8283fab983dc6e200d7acf569883940524003973edfc1015756|dev=recipe_err=MissingColourData"),
    ("raw-vendor", "canon-eos-r6.cr3", "decode_err=DecoderUnavailable(\"cr3\")"),
    ("raw-vendor", "fujifilm-finepix-s5000.raf", "decode_err=DecoderUnavailable(\"raf\")"),
    ("raw-vendor", "fujifilm-x-s10-14bit-compressed-xtrans.raf", "decode_err=DecoderUnavailable(\"raf\")"),
    ("raw-vendor", "nikon-d1h-12bit-uncompressed.nef", "2012x1324|12|cfa=25c44149c9b10f0b11ec8e99d4ee6f1bdfe2b1095cc7bbeebb5809901fbbdbba|dev=recipe_err=MissingColourData"),
    ("raw-vendor", "nikon-d70s-12bit-lossy-type1.nef", "decode_err=DecoderUnavailable(\"nef_compressed\")"),
    ("raw-vendor", "nikon-z6-12bit-compressed.nef", "decode_err=DecoderUnavailable(\"nef_compressed\")"),
    ("raw-vendor", "olympus-c5060wz-12bit.orf", "decode_err=DecoderUnavailable(\"orf\")"),
    ("raw-vendor", "panasonic-dmc-lx7.rw2", "decode_err=DecoderUnavailable(\"rw2\")"),
    ("raw-vendor", "sony-ilce-7s-14bit-compressed.arw", "2816x1872|14|cfa=3fd53781275f0e7c114ed54873071d49cc6f7677090445dc0e92fbd9feebe8a2|dev=recipe_err=MissingColourData"),
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2).expect("crates/<crate>").to_path_buf()
}

fn convention_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
    })
}

fn corpus_root() -> Result<PathBuf, String> {
    let ws = workspace_root();
    let text = std::fs::read_to_string(ws.join(".config").join("oracle-corpus.toml"))
        .map_err(|e| format!("corpus_convention_absent error={e}"))?;
    let search = convention_value(&text, "search").ok_or("corpus_convention_malformed key=search")?;
    let max: usize = convention_value(&text, "max-ancestors")
        .and_then(|v| v.parse().ok())
        .ok_or("corpus_convention_malformed key=max-ancestors")?;
    ws.ancestors()
        .take(max + 1)
        .map(|a| a.join(&search))
        .find(|c| c.is_dir())
        .ok_or_else(|| format!("corpus_root_absent search={search}"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn profile() -> ProfileBinding {
    ProfileBinding {
        profile_id: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap(),
        sha256: [0; 32],
    }
}

/// One line: `dims|bits|cfa_sha256|developed_sha256` or the typed error.
fn ident(bytes: &[u8]) -> String {
    let raw = match decode_container(bytes) {
        Ok(raw) => raw,
        Err(e) => return format!("decode_err={e:?}"),
    };
    let mut cfa = Sha256::new();
    for y in 0..raw.height {
        for x in 0..raw.width {
            cfa.update(raw.sample(x, y).to_le_bytes());
        }
    }
    let cfa = hex(&cfa.finalize());
    let window = raw.default_window();
    let developed = (|| {
        let (plane, report) = raw
            .cfa_plane(window, RangePolicy::ClampReported, 1, Limits::LARGE)
            .map_err(|e| format!("plane_err={e:?}"))?;
        let (recipe, _) = raw
            .recipe(&hsk_studio_develop::color::xyz_d50_to_linear_srgb().unwrap(), profile(), BaselineExposure::Ignore)
            .map_err(|e| format!("recipe_err={e:?}"))?;
        let (image, _) = develop_to_image(&plane, &recipe, 1, &CancellationToken::default())
            .map_err(|e| format!("develop_err={e:?}"))?;
        let mut h = Sha256::new();
        for v in &image.rgb {
            h.update(v.to_le_bytes());
        }
        Ok::<_, String>(format!("{}x{}:{:?}:{}", image.width, image.height, (report.clamped_below, report.clamped_above), hex(&h.finalize())))
    })();
    let developed = developed.unwrap_or_else(|e| e);
    format!("{}x{}|{}|cfa={cfa}|dev={developed}", raw.width, raw.height, raw.bits_per_sample)
}

#[test]
#[ignore = "needs the oracle corpus (dng/, raw-vendor/; CC0 downloads, never committed); NOT_PROVEN when absent"]
fn container_identity_over_the_corpus() {
    let root = match corpus_root() {
        Ok(root) => root,
        Err(reason) => {
            println!("NOT_PROVEN: container_identity_over_the_corpus reason={reason}");
            return;
        }
    };
    let mut mismatches = Vec::new();
    for sub in ["dng", "raw-vendor"] {
        let mut files: Vec<_> = std::fs::read_dir(root.join(sub))
            .map(|d| d.filter_map(|e| e.ok()).map(|e| e.path()).collect())
            .unwrap_or_default();
        files.sort();
        for path in files {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if name.ends_with(".json") {
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            let got = ident(&bytes);
            println!("IDENT {sub}/{name} {got}");
            if let Some((_, _, want)) = PINNED.iter().find(|(s, n, _)| *s == sub && *n == name)
                && *want != got
            {
                mismatches.push(format!("{sub}/{name}: want {want} got {got}"));
            }
        }
    }
    assert!(mismatches.is_empty(), "identity broken: {mismatches:#?}");
}
