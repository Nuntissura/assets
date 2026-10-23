//! Entry-point agreement on the sample-rate rule (PLR-054, lane SMB-SCORE-PLUGIN-RATE).
//!
//! Three places in this crate decide whether a configured sample rate is acceptable: the host's
//! `ProcessConfig::validate` and the `configure` of the two worker backends (reference and, with feature
//! `clap`, CLAP). They once carried three copies of a `0 < rate <= MAX` check, so a 44_101 Hz
//! configuration was accepted at activation and only failed at the first block, where `hsk-studio-score`
//! refuses a rate that is not an exact tick divisor. Each test pins one entry point to the rule
//! `hsk_studio_score::ticks_per_sample` applies, over the same hand-derived table, so the three agree.
#![allow(clippy::arithmetic_side_effects)] // test code
use crate::backend::{Backend, Reference};
use crate::error::PluginError;
use crate::types::{
    ApprovedPlugin, Arch, Completeness, PluginFormat, ProcessConfig, SUPPORTED_CLAP_ABI,
};
use crate::wire::{ConfigureSpec, E_CONFIG, PluginSpec};
use hsk_studio_score::{ChannelLayout, sha256, ticks_per_sample};
use std::path::PathBuf;

/// (rate in Hz, admitted by the score rule). The score rule: `1 <= rate <= 768_000` and the rate divides
/// the tick rate 254_016_000_000 = 2^12 * 3^4 * 5^6 * 7^2; the factorisations are worked by hand.
const RATES: &[(u32, bool)] = &[
    // Admitted: every prime factor of the rate is 2, 3, 5 or 7 within the tick rate's powers.
    (1, true),       // 1
    (8_000, true),   // 2^6 * 5^3
    (11_025, true),  // 3^2 * 5^2 * 7^2
    (16_000, true),  // 2^7 * 5^3
    (22_050, true),  // 2 * 3^2 * 5^2 * 7^2
    (24_000, true),  // 2^6 * 3 * 5^3
    (32_000, true),  // 2^8 * 5^3
    (44_100, true),  // 2^2 * 3^2 * 5^2 * 7^2
    (48_000, true),  // 2^7 * 3 * 5^3
    (88_200, true),  // 2^3 * 3^2 * 5^2 * 7^2
    (96_000, true),  // 2^8 * 3 * 5^3
    (176_400, true), // 2^4 * 3^2 * 5^2 * 7^2
    (192_000, true), // 2^9 * 3 * 5^3
    (352_800, true), // 2^5 * 3^2 * 5^2 * 7^2
    (384_000, true), // 2^10 * 3 * 5^3
    (500_000, true), // 2^5 * 5^6, the full power of 5
    (705_600, true), // 2^6 * 3^2 * 5^2 * 7^2
    (768_000, true), // 2^11 * 3 * 5^3, the largest admitted rate
    // Refused: zero.
    (0, false),
    // Refused: a prime factor outside {2, 3, 5, 7}.
    (44_101, false), // coprime to 2, 3, 5 and 7 (44_100 + 1): the PLR-054 case
    (47_999, false), // 7 * 6_857, and 6_857 is coprime to 2, 3, 5 and 7
    (22_051, false), // coprime to 2, 3, 5 and 7 (22_050 + 1)
    (96_001, false), // coprime to 2, 3, 5 and 7 (96_000 + 1)
    (11_000, false), // 2^3 * 5^3 * 11
    // Refused: above the authored bound, even when an exact divisor (1_000_000 = 2^6 * 5^6,
    // 1_536_000 = 2^12 * 3 * 5^3).
    (768_001, false),
    (1_000_000, false),
    (1_536_000, false),
    (u32::MAX, false),
];

fn stereo_config(sample_rate_hz: u32) -> ProcessConfig {
    ProcessConfig {
        plugin: ApprovedPlugin {
            path: PathBuf::from("not-read-by-validate"),
            content_sha256: [0; 32],
            format: PluginFormat::Clap,
            arch: Arch::host().expect("known arch"),
            abi_version: SUPPORTED_CLAP_ABI,
        },
        grant: None,
        sample_rate_hz,
        layout: ChannelLayout::stereo(),
        max_block_frames: 256,
        parameters: Vec::new(),
        max_state_bytes: 64,
    }
}

fn configure_spec(sample_rate_hz: u32) -> ConfigureSpec {
    ConfigureSpec {
        sample_rate_hz,
        channels: 2,
        max_frames: 256,
        params: Vec::new(),
    }
}

fn plugin_spec(path: &str, sha: [u8; 32]) -> PluginSpec {
    PluginSpec {
        format: PluginFormat::Clap.wire(),
        arch: Arch::host().map_or(0, Arch::wire),
        abi: SUPPORTED_CLAP_ABI,
        sha,
        path: path.to_owned(),
    }
}

/// A small approved "plugin" file the reference backend can hash; removed when dropped.
struct TempFile(PathBuf);

impl TempFile {
    fn new(bytes: &[u8]) -> Self {
        let path = std::env::temp_dir().join(format!(
            "hsk-score-plugin-rate-gate-{}.bin",
            std::process::id()
        ));
        std::fs::write(&path, bytes).unwrap();
        Self(path)
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The table is the score rule: if `hsk-studio-score` ever changes which rates it admits, every test
/// below fails here before an entry point can drift from it.
fn table_is_the_score_rule() {
    for &(rate, admitted) in RATES {
        assert_eq!(
            ticks_per_sample(rate).is_ok(),
            admitted,
            "score rule for {rate} Hz disagrees with the table"
        );
    }
    // Hand-computed ticks per sample pin the oracle itself (254_016_000_000 / rate).
    assert_eq!(ticks_per_sample(48_000), Ok(5_292_000));
    assert_eq!(ticks_per_sample(44_100), Ok(5_760_000));
    assert_eq!(ticks_per_sample(768_000), Ok(330_750));
}

/// Host entry point: `ProcessConfig::validate` (run by `activate` before any process exists).
#[test]
fn host_validate_follows_the_score_rule() {
    table_is_the_score_rule();
    for &(rate, admitted) in RATES {
        let want = if admitted {
            Ok(())
        } else {
            Err(PluginError::InvalidConfig)
        };
        assert_eq!(stereo_config(rate).validate(), want, "{rate} Hz");
    }
}

/// Worker entry point of the reference backend: `Reference::configure` after the plugin file verified.
#[test]
fn reference_configure_follows_the_score_rule() {
    table_is_the_score_rule();
    let file = TempFile::new(b"abc");
    let approved = plugin_spec(file.0.to_str().unwrap(), sha256(b"abc"));
    for &(rate, admitted) in RATES {
        let got = Reference::new(0, Completeness::Complete)
            .configure(&approved, &configure_spec(rate))
            .map(|_| ());
        let want = if admitted { Ok(()) } else { Err((E_CONFIG, 0)) };
        assert_eq!(got, want, "{rate} Hz");
    }
}

/// Worker entry point of the CLAP backend: `ClapBackend::configure`. An absent file and no staging
/// directory mean a rate that passes the gate stops at `E_UNAVAILABLE` (nothing is staged, hashed or
/// loaded); a refused rate stops at `E_CONFIG` before that.
#[cfg(feature = "clap")]
#[test]
fn clap_configure_follows_the_score_rule() {
    table_is_the_score_rule();
    let unstaged = plugin_spec("not-a-file", [0; 32]);
    for &(rate, admitted) in RATES {
        let got = crate::clap_backend::ClapBackend::new(None)
            .configure(&unstaged, &configure_spec(rate))
            .map(|_| ());
        let want = if admitted {
            Err((crate::wire::E_UNAVAILABLE, 0))
        } else {
            Err((E_CONFIG, 0))
        };
        assert_eq!(got, want, "{rate} Hz");
    }
}
