//! V4-01/V4-02 hostile sizes for the wide decoder (SMB-PRISM-ENG-6): declared dimensions at the u32 limit
//! and aggregates over the budget are refused with typed errors before any large allocation or parse, never
//! a panic or an out-of-memory abort.
mod wide_support;
use hsk_studio_accord::CancellationToken;
use hsk_studio_prism::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
use wide_support::*;

fn inspect(bytes: &[u8]) -> Result<WideProfileDescriptor, WideError> {
    let ctx = Ctx::new();
    engine(&ctx).inspect_profile(ctx.input(&ctx.source, bytes), &CancellationToken::default())
}
fn mab_header(clut_grid: [u8; 3]) -> Vec<u8> {
    // mAB, 3 in / 3 out, CLUT at 32 (grid, precision 2) and nothing after: the declared CLUT is what counts.
    let mut t = b"mAB \0\0\0\0".to_vec();
    t.extend_from_slice(&[3, 3, 0, 0]);
    t.extend_from_slice(&be32(32)); // B curves
    t.extend_from_slice(&be32(0)); // matrix
    t.extend_from_slice(&be32(0)); // M curves
    t.extend_from_slice(&be32(32)); // CLUT
    t.extend_from_slice(&be32(0)); // A curves
    let mut grid = [0u8; 16];
    grid[..3].copy_from_slice(&clut_grid);
    t.extend_from_slice(&grid);
    t.extend_from_slice(&[2, 0, 0, 0]);
    t
}

#[test]
fn dims_u32_max_and_oversized_tables_are_refused_typed() {
    // Tag table: offset + size past u32 range / past the file.
    let mut huge_tag = matrix_v2(2.2);
    huge_tag[132 + 4..132 + 8].copy_from_slice(&be32(u32::MAX - 4));
    huge_tag[132 + 8..132 + 12].copy_from_slice(&be32(u32::MAX));
    assert_eq!(inspect(&huge_tag).unwrap_err(), WideError::MalformedProfile);
    // Tag count u32::MAX.
    let mut many = matrix_v2(2.2);
    many[128..132].copy_from_slice(&be32(u32::MAX));
    assert_eq!(inspect(&many).unwrap_err(), WideError::MalformedProfile);
    // `curv` declaring u32::MAX entries.
    let mut curve = b"curv\0\0\0\0".to_vec();
    curve.extend_from_slice(&be32(u32::MAX));
    curve.extend_from_slice(&[0, 0, 0xff, 0xff]);
    let blobs = vec![xyz_tag(D50), xyz_tag([0.4360, 0.2225, 0.0139]), xyz_tag([0.3851, 0.7169, 0.0971]), xyz_tag([0.1431, 0.0606, 0.7141]), curve];
    let tags: [(&[u8; 4], usize); 7] = [(b"wtpt", 0), (b"rXYZ", 1), (b"gXYZ", 2), (b"bXYZ", 3), (b"rTRC", 4), (b"gTRC", 4), (b"bTRC", 4)];
    let huge_curve = assemble(0x0210_0000, b"mntr", b"RGB ", b"XYZ ", &blobs, &tags);
    assert_eq!(inspect(&huge_curve).unwrap_err(), WideError::MalformedProfile);
    // mft2 CMYK -> Lab declaring a 255^4 CLUT (4.2e9 nodes) with no data behind it.
    let mut lut = b"mft2\0\0\0\0".to_vec();
    lut.extend_from_slice(&[4, 3, 255, 0]);
    for v in [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0] {
        lut.extend_from_slice(&s15(v));
    }
    lut.extend_from_slice(&[0, 2, 0, 2]);
    let huge_lut = assemble(0x0210_0000, b"prtr", b"CMYK", b"Lab ", &[xyz_tag(D50), lut], &[(b"wtpt", 0), (b"A2B0", 1)]);
    assert_eq!(inspect(&huge_lut).unwrap_err(), WideError::MalformedProfile);
    // mAB RGB -> XYZ declaring a 255^3 x 3 CLUT (49.7M values) with no data behind it.
    let huge_mab = assemble(0x0420_0000, b"mntr", b"RGB ", b"XYZ ", &[xyz_tag(D50), mab_header([255, 255, 255])], &[(b"wtpt", 0), (b"A2B0", 1)]);
    assert_eq!(inspect(&huge_mab).unwrap_err(), WideError::MalformedProfile);
    // Whole profile over the 2 MiB bound: refused on size alone.
    let mut big = matrix_v2(2.2);
    big.resize(MAX_WIDE_PROFILE_BYTES + 4, 0);
    let len = big.len() as u32;
    big[..4].copy_from_slice(&be32(len));
    assert_eq!(inspect(&big).unwrap_err(), WideError::ProfileLimit);
}

/// `mAB `/`mBA ` tag with explicit (hostile) channel counts: no curves, no matrix, and for `Some(grid)` a CLUT whose
/// 16 grid sizes are all `grid` (precision 2, zero data of the length moxcms reads: the product of the first
/// min(inputs, 16) grid sizes times the output channels).
fn mab_counts(sig: &[u8; 4], inputs: u8, outputs: u8, grid: Option<u8>) -> Vec<u8> {
    let mut t = sig.to_vec();
    t.extend_from_slice(&[0; 4]);
    t.extend_from_slice(&[inputs, outputs, 0, 0]);
    t.extend_from_slice(&be32(0)); // B curves
    t.extend_from_slice(&be32(0)); // matrix
    t.extend_from_slice(&be32(0)); // M curves
    t.extend_from_slice(&be32(if grid.is_some() { 32 } else { 0 })); // CLUT
    t.extend_from_slice(&be32(0)); // A curves
    if let Some(g) = grid {
        t.extend_from_slice(&[g; 16]);
        t.extend_from_slice(&[2, 0, 0, 0]);
        let nodes = usize::from(g).pow(u32::from(inputs.min(16))) * usize::from(outputs);
        t.resize(t.len() + nodes * 2, 0);
    }
    t
}
fn rgb_with_table(sig: &[u8; 4], table: Vec<u8>) -> Vec<u8> {
    assemble(0x0420_0000, b"mntr", b"RGB ", b"XYZ ", &[xyz_tag(D50), table], &[(b"wtpt", 0), (sig, 1)])
}
/// Which typed error a one-pixel conversion through a plan built from the profile ends in.
fn plan_error(source: &[u8], destination: &[u8]) -> WideError {
    let ctx = Ctx::new();
    let mut eng = engine(&ctx);
    match run(&mut eng, &ctx, source, destination, &[0.5, 0.5, 0.5], Intent::RelativeColorimetric, false) {
        Ok(_) => panic!("hostile table was admitted"),
        Err(f) => f.error,
    }
}

/// R6 review A (icc_eval.rs Table::build): the input-channel byte of a `mAB `/`mBA ` tag (0..=255) used to slice the
/// 16-entry grid array unchecked, and moxcms 0.9.1 parses a tag with up to 255 inputs when it has at most 4
/// outputs. Every such count must end in a typed refusal, never a panic.
#[test]
fn mab_input_channel_counts_beyond_the_icc_clut_array_are_refused_typed() {
    // Source side (A2B0 `mAB `, RGB device): 17, 64 and 255 inputs with a CLUT, and without one.
    for inputs in [17u8, 64, 255] {
        let with_clut = rgb_with_table(b"A2B0", mab_counts(b"mAB ", inputs, 3, Some(2)));
        assert_eq!(plan_error(&with_clut, SRGB), WideError::MalformedProfile, "A2B0 {inputs} inputs with CLUT");
        let no_clut = rgb_with_table(b"A2B0", mab_counts(b"mAB ", inputs, 3, None));
        assert_eq!(plan_error(&no_clut, SRGB), WideError::MalformedProfile, "A2B0 {inputs} inputs no CLUT");
    }
    // Destination side (B2A0 `mBA `): the same counts in the PCS -> device table.
    for inputs in [17u8, 255] {
        let with_clut = rgb_with_table(b"B2A0", mab_counts(b"mBA ", inputs, 3, Some(2)));
        assert_eq!(plan_error(SRGB, &with_clut), WideError::MalformedProfile, "B2A0 {inputs} inputs with CLUT");
    }
    // Inside the ICC array (<= 16) but beyond Prism's own 8-input CLUT bound: unsupported, not malformed.
    for inputs in [9u8, 16] {
        let table = rgb_with_table(b"A2B0", mab_counts(b"mAB ", inputs, 3, Some(2)));
        assert_eq!(plan_error(&table, SRGB), WideError::UnsupportedProfile, "A2B0 {inputs} inputs");
    }
    // Output channel counts: 17 and 255 outputs (3 inputs) exceed the evaluator's 16-channel scratch.
    for outputs in [17u8, 255] {
        let table = rgb_with_table(b"A2B0", mab_counts(b"mAB ", 3, outputs, Some(2)));
        assert_eq!(plan_error(&table, SRGB), WideError::UnsupportedProfile, "A2B0 {outputs} outputs");
    }
    // A table whose channel counts differ from the device (5 inputs on an RGB profile) is malformed.
    let five = rgb_with_table(b"A2B0", mab_counts(b"mAB ", 5, 3, Some(2)));
    assert_eq!(plan_error(&five, SRGB), WideError::MalformedProfile);
}

struct CappedLedger {
    live: AtomicU64,
    cap: u64,
}
#[derive(Clone)]
struct Capped(Arc<CappedLedger>);
struct CappedLease(Arc<CappedLedger>, u64);
impl Drop for CappedLease {
    fn drop(&mut self) {
        self.0.live.fetch_sub(self.1, SeqCst);
    }
}
impl ProviderReservation for CappedLease {
    fn reserved_bytes(&self) -> u64 {
        self.1
    }
}
impl ProviderAdmission for Capped {
    type Reservation = CappedLease;
    fn synchronous_retirement(&self) -> bool {
        true
    }
    fn reserve(&self, bytes: u64) -> Result<CappedLease, Error> {
        let live = self.0.live.load(SeqCst);
        if live.checked_add(bytes).is_none_or(|t| t > self.0.cap) {
            return Err(Error::AdmissionDenied);
        }
        self.0.live.fetch_add(bytes, SeqCst);
        Ok(CappedLease(self.0.clone(), bytes))
    }
}

#[test]
fn aggregate_over_budget_is_denied_before_any_parse() {
    if !cfg!(all(target_arch = "x86_64", target_os = "windows", target_env = "msvc")) {
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), "NOT_PROVEN: wide_hostile aggregate_over_budget reason=admission_route_x86_64_windows_msvc_only");
        return;
    }
    let ctx = Ctx::new();
    let cmyk = cmyk_k_profile(9, 3, 20.0);
    let pixels = vec![0.25f32; 4 * 64];
    // Budget: the provider lease plus 64 KiB, far below the plan bound of a CMYK + sRGB pair.
    let ledger = Arc::new(CappedLedger { live: AtomicU64::new(0), cap: u64::MAX });
    let probe = AdmittedWideEngine::new(&ctx.actor, 2, Capped(ledger.clone())).unwrap();
    let provider = probe.provider_reserved_bytes();
    drop(probe);
    let ledger = Arc::new(CappedLedger { live: AtomicU64::new(0), cap: provider + 64 * 1024 });
    let mut eng = AdmittedWideEngine::new(&ctx.actor, 2, Capped(ledger.clone())).unwrap();
    let denied = eng
        .transform(&ctx.request(&cmyk, SRGB, &pixels, Intent::RelativeColorimetric, true), &CancellationToken::default(), &mut Collect(Vec::new()))
        .err()
        .expect("plan over budget");
    assert!(matches!(denied, AdmittedWideError::Admission(Error::AdmissionDenied)), "{denied:?}");
    assert_eq!((eng.cache_stats().parses, eng.cache_stats().entries), (0, 0));
    assert_eq!(ledger.live.load(SeqCst), provider);
    // Pixel count over the request bound: refused before any charge.
    let too_many = vec![0.0f32; (MAX_WIDE_PIXELS + 1) * 4];
    let refused = eng
        .transform(&ctx.request(&cmyk, SRGB, &too_many, Intent::RelativeColorimetric, false), &CancellationToken::default(), &mut Collect(Vec::new()))
        .err()
        .expect("pixel bound");
    assert!(matches!(refused, AdmittedWideError::Admission(Error::PixelLimit)), "{refused:?}");
    assert_eq!(ledger.live.load(SeqCst), provider);
}
