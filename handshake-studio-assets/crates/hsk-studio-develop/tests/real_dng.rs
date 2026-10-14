//! Real-camera DNG validation against published CC0 samples (raw.pixls.us). `#[ignore]`d: the
//! samples are test DATA fetched on demand into the oracle corpus (never committed, never executed);
//! run with `cargo nextest run -p hsk-studio-develop --test real_dng --run-ignored ignored-only --no-capture`
//! (the workspace runs tests through that runner for the per-test timeouts of `.config/nextest.toml`).
//!
//! Corpus dir: `$STUDIO_DNG_CORPUS`, else `<Worktrees>/Handshake_Artifacts/WP-KERNEL-STUDIO/oracle-corpus/dng`
//! (relative to this crate). Files and checksums: see `real_dng/fetch-corpus.sh` and the table below.
//! Independent oracles: (1) the Exiv2 metadata dump raw.pixls.us publishes per sample (values copied
//! below before any download), (2) ExifTool when `$STUDIO_EXIFTOOL` names its executable.
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_develop::*;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::PathBuf, process::Command, time::Instant};

struct Facts {
    file: &'static str,
    sha256: &'static str,
    width: u32,
    height: u32,
    bits: u16,
    /// TIFF Compression value (1 none, 7 lossless JPEG).
    compression: u32,
    cfa: Bayer,
    black: &'static [f64],
    white: u32,
    active: (u32, u32, u32, u32),
    default_crop: Option<(u32, u32, u32, u32)>,
    linearized: bool,
    illuminants: (Option<u16>, Option<u16>),
    /// `unapplied` entries the reader must report.
    unapplied: &'static [&'static str],
    /// Percent of default-window samples that may be clamped (below black / above white). Sensor
    /// noise around black in a dark frame clamps below black; 1 unless the file's own data shows
    /// otherwise (measured value recorded at the entry).
    clamp_pct_max: u64,
    /// The downloaded file is not decodable as the facts describe: the exact typed refusal
    /// expected instead (the case then proves only the refusal and prints NOT_PROVEN).
    refusal: Option<(DevelopError, &'static str)>,
}

const CORPUS: &[Facts] = &[
    // Canon EOS 5D Mark III, MLV App export: single-strip lossless JPEG, 14-bit, two profiles + FM.
    Facts {
        file: "canon-eos-5d3-mlv-14bit-lossless.dng",
        sha256: "1d77dcc6cdb839b10c1948f0b4471984ab48cf15e973fb77520584cee72b603e",
        width: 1920,
        height: 818,
        bits: 14,
        compression: 7,
        cfa: Bayer::Rggb,
        black: &[2145.0],
        white: 16200,
        active: (0, 0, 818, 1920),
        default_crop: Some((0, 0, 1920, 818)),
        linearized: false,
        illuminants: (Some(17), Some(21)),
        unapplied: &[],
        // Dark MLV frame: 11.3% of samples sit below the authored black 2145 (decoded min 2052,
        // p5 2104, p25 2233, p50 2459; lane SMB-DEVELOP-DNG-MLV probe 2026-10-14), i.e. read noise
        // around black, not a decode fault (same-colour steps below prove the row assembly).
        clamp_pct_max: 15,
        refusal: None,
    },
    // Samsung SM-G973U: uncompressed 16-bit, one-row strips, 2x2 black repeat, GainMap opcode list 2.
    Facts {
        file: "samsung-sm-g973u-16bit-uncompressed.dng",
        sha256: "6492b7e0ac8e012524c0c83606a104cc9bf3b68f30fa60c7b73ffbe4dc584ab1",
        width: 4032,
        height: 1908,
        bits: 16,
        compression: 1,
        cfa: Bayer::Grbg,
        black: &[0.0, 0.0, 0.0, 0.0],
        white: 1023,
        active: (0, 0, 1908, 4032),
        default_crop: Some((8, 8, 4016, 1892)),
        linearized: false,
        illuminants: (Some(20), Some(17)),
        unapplied: &["opcode_list_2"],
        clamp_pct_max: 1,
        // The CC0 download (raw.pixls.us id 3839, sha256 = the listing checksum) is a baseline JFIF
        // rendering (SOF0 4016x1892x3, 8-bit) whose Exif APP1 carries this DNG's IFD0 tags but no
        // raw strips (byte inspection, lane SMB-DEVELOP-DNG-MLV 2026-10-14): not a DNG. The facts
        // above stay as the published Exiv2 values for a replacement sample.
        refusal: Some((
            DevelopError::DecoderUnavailable("jpeg"),
            "listed_download_is_a_jfif_rendering_not_a_dng",
        )),
    },
    // Nikon D80: raw in a SubIFD (IFD0 = thumbnail), 256x240 lossless-JPEG tiles, LinearizationTable.
    Facts {
        file: "nikon-d80-16bit-lossless-tiled-linearized.dng",
        sha256: "5095ca264e763624cd3aa62940286dd1bcff4504eefa1ce61cbb72f50f8426bb",
        width: 3900,
        height: 2611,
        bits: 16,
        compression: 7,
        cfa: Bayer::Gbrg,
        black: &[0.0],
        white: 3880,
        active: (0, 0, 2611, 3900),
        default_crop: Some((14, 9, 3872, 2592)),
        linearized: true,
        // IFD0 CalibrationIlluminant1/2 (0xC65A/0xC65B) = 17, 21 in the downloaded file (byte
        // inspection, lane SMB-DEVELOP-DNG-MLV 2026-10-14); the pre-download entry said none.
        illuminants: (Some(17), Some(21)),
        unapplied: &[],
        clamp_pct_max: 1,
        refusal: None,
    },
];

/// Default corpus location relative to this crate: the sibling `Handshake_Artifacts` root next to
/// the worktrees, outside the git worktree. `real_dng/fetch-corpus.sh` carries the identical string;
/// `fetch_script_default_matches_test_default` keeps the two from drifting.
const CORPUS_RELATIVE: &str = "../../../../Handshake_Artifacts/WP-KERNEL-STUDIO/oracle-corpus/dng";

fn corpus_dir() -> PathBuf {
    match std::env::var_os("STUDIO_DNG_CORPUS") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(CORPUS_RELATIVE),
    }
}

/// The fetch script and this harness must resolve the same default (R3 finding: they differed by
/// one directory level, so an approved fetch would have landed inside the repo).
#[test]
fn fetch_script_default_matches_test_default() {
    let script = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/real_dng/fetch-corpus.sh"),
    )
    .unwrap()
    .replace("\r\n", "\n");
    let relative = format!("CORPUS_RELATIVE=\"{CORPUS_RELATIVE}\"\n");
    assert!(script.contains(&relative), "script must define {relative}");
    // The script resolves it from the crate directory (HERE/../..), exactly as CARGO_MANIFEST_DIR.
    assert!(script.contains("CRATE=\"$(cd \"$HERE/../..\" && pwd)\"\n"));
    assert!(script.contains("\"$CRATE/$CORPUS_RELATIVE\""));
    assert!(!script.contains("$HERE/../../../../.."), "no second, independent default");
}

/// `exiftool -a -s -G1 -n` -> (group, tag, value) in file order.
fn exiftool(path: &PathBuf) -> Option<Vec<(String, String, String)>> {
    let exe = std::env::var_os("STUDIO_EXIFTOOL")?;
    let out = Command::new(exe).args(["-a", "-s", "-G1", "-n"]).arg(path).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    Some(
        text.lines()
            .filter_map(|line| {
                let (left, value) = line.split_once(" : ")?;
                let (group, rest) = left.strip_prefix('[')?.split_once(']')?;
                Some((group.to_owned(), rest.trim().to_owned(), value.trim().to_owned()))
            })
            .collect(),
    )
}

fn profile() -> ProfileBinding {
    ProfileBinding {
        profile_id: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap(),
        sha256: [0; 32],
    }
}

#[test]
#[ignore = "needs the CC0 sample corpus (fetch-corpus.sh); run explicitly with --ignored"]
fn real_dng_decode_and_develop() {
    let dir = corpus_dir();
    let mut summary = Vec::new();
    for facts in CORPUS {
        let path = dir.join(facts.file);
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e} (run fetch-corpus.sh --go)", path.display()));
        let sha: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(sha, facts.sha256, "{}: corpus file differs from the published checksum", facts.file);

        if let Some((error, reason)) = facts.refusal {
            assert_eq!(decode_container(&bytes).err(), Some(error), "{}", facts.file);
            println!("NOT_PROVEN: real_dng_decode_and_develop {} reason={reason}", facts.file);
            summary.push(format!("{}: refused {error} (expected: {reason})", facts.file));
            continue;
        }
        let t = Instant::now();
        let raw = decode_container(&bytes).unwrap_or_else(|e| panic!("{}: decode refused: {e}", facts.file));
        let decode_ms = t.elapsed().as_millis();
        assert_eq!((raw.width, raw.height, raw.bits_per_sample), (facts.width, facts.height, facts.bits), "{}", facts.file);
        assert_eq!(raw.cfa_pattern, CfaPattern::Bayer(facts.cfa), "{}", facts.file);
        assert_eq!(raw.black_level, facts.black, "{}", facts.file);
        assert_eq!(raw.white_level, facts.white, "{}", facts.file);
        let a = raw.active_area;
        assert_eq!((a.top, a.left, a.bottom, a.right), facts.active, "{}", facts.file);
        let window = raw.default_window();
        if let Some((x, y, w, h)) = facts.default_crop {
            let cx = raw.active_area.left + x;
            let cy = raw.active_area.top + y;
            assert_eq!((window.x, window.y, window.width, window.height), (cx, cy, w, h), "{}", facts.file);
        }
        assert_eq!(raw.linearized, facts.linearized, "{}", facts.file);
        assert_eq!((raw.color.calibration_illuminant[0], raw.color.calibration_illuminant[1]), facts.illuminants, "{}", facts.file);
        for name in facts.unapplied {
            assert!(raw.unapplied.contains(name), "{}: expected unapplied {name}, got {:?}", facts.file, raw.unapplied);
        }

        // Second oracle: ExifTool values of the raw IFD (when STUDIO_EXIFTOOL is set).
        let mut oracle = "exiv2-table".to_owned();
        if let Some(lines) = exiftool(&path) {
            let group = lines
                .iter()
                .find(|(_, tag, value)| tag == "PhotometricInterpretation" && value == "32803")
                .map(|(g, _, _)| g.clone())
                .expect("exiftool: no CFA IFD");
            let in_group: BTreeMap<&str, &str> =
                lines.iter().filter(|(g, _, _)| *g == group).map(|(_, t, v)| (t.as_str(), v.as_str())).collect();
            let num = |tag: &str| in_group.get(tag).and_then(|v| v.split_whitespace().next()).and_then(|v| v.parse::<f64>().ok());
            assert_eq!(num("ImageWidth"), Some(f64::from(facts.width)), "exiftool width");
            assert_eq!(num("ImageHeight"), Some(f64::from(facts.height)), "exiftool height");
            assert_eq!(num("BitsPerSample"), Some(f64::from(facts.bits)), "exiftool bits");
            assert_eq!(num("Compression"), Some(f64::from(facts.compression)), "exiftool compression");
            assert_eq!(num("WhiteLevel"), Some(f64::from(facts.white)), "exiftool white");
            assert_eq!(num("BlackLevel"), Some(facts.black[0]), "exiftool black");
            oracle = format!("exiftool group {group} + exiv2-table");
        }

        // Develop the default window end to end with the LARGE limits (explicit, counted clamping).
        let (plane, report) = raw.cfa_plane(window, RangePolicy::ClampReported, 1, Limits::LARGE).unwrap();
        let samples = u64::from(window.width) * u64::from(window.height);
        let (recipe, solution) = raw
            .recipe(&hsk_studio_develop::color::xyz_d50_to_linear_srgb().unwrap(), profile(), BaselineExposure::Ignore)
            .unwrap_or_else(|e| panic!("{}: colour refused: {e}", facts.file));
        let t = Instant::now();
        let (image, receipt) = develop_to_image(&plane, &recipe, 1, &CancellationToken::default())
            .unwrap_or_else(|e| panic!("{}: develop refused: {e}", facts.file));
        let develop_ms = t.elapsed().as_millis();
        assert_eq!((image.width, image.height), (window.width, window.height));
        assert!(image.rgb.iter().all(|v| v.is_finite()), "{}: non-finite output", facts.file);
        let mean = image.rgb.iter().map(|v| f64::from(*v)).sum::<f64>() / image.rgb.len() as f64;
        assert!(mean > 0.0 && mean < 4.0, "{}: implausible mean {mean}", facts.file);
        // Clamping stays within the file's measured budget (sensor noise at black), not the rule.
        assert!(
            (report.clamped_below + report.clamped_above) * 100 < samples * facts.clamp_pct_max,
            "{}: {report:?} over {}%",
            facts.file,
            facts.clamp_pct_max
        );
        // Row assembly check independent of levels: same-colour neighbours two rows apart differ
        // about as much as same-colour neighbours two columns apart (a misassembled strip or
        // lossless-JPEG row split makes the vertical step explode).
        let (mut hstep, mut vstep) = (0u64, 0u64);
        for y in (0..raw.height - 2).step_by(7) {
            for x in (0..raw.width - 2).step_by(5) {
                hstep += u64::from(raw.sample(x, y).abs_diff(raw.sample(x + 2, y)));
                vstep += u64::from(raw.sample(x, y).abs_diff(raw.sample(x, y + 2)));
            }
        }
        assert!(vstep < hstep.max(1) * 3 && hstep < vstep.max(1) * 3, "{}: hstep {hstep} vstep {vstep}", facts.file);
        summary.push(format!(
            "{}: {}x{} bits={} comp={} cfa={:?} decode_ms={decode_ms} window={}x{} clamped_below={} clamped_above={} \
             cct={:.0} fm={} develop_ms={develop_ms} tiles={} mean={mean:.4} unapplied={:?} oracle={oracle}",
            facts.file, raw.width, raw.height, raw.bits_per_sample, facts.compression, facts.cfa, window.width,
            window.height, report.clamped_below, report.clamped_above, solution.cct, solution.used_forward_matrix,
            receipt.tiles, raw.unapplied
        ));
    }
    println!("{}", summary.join("\n"));
}
