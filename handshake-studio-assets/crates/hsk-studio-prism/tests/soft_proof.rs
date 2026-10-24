//! Soft proofing (SMB-PS-SOFT-PROOF-1): proof condition record and presets (STU-COL-241/242/243), the
//! two-intent proof chain (STU-COL-022/023/240), colour-vision conditions (STU-COL-243), gamut warning and
//! in-gamut substitute (STU-COL-244/245/138). Profiles: CC0 `tests/profiles/sRGB-v4.icc` and
//! `DisplayP3-v4.icc`, project-made synthetic `tests/oracle/profiles/hsk-synth-cmyk-v2.icc`.
//!
//! NOT_PROVEN (printed by `not_proven_parity_lines`): equality with Photoshop View > Proof Colors / Gamut
//! Warning output and with LittleCMS proofing transforms (no proof case in either oracle corpus).
mod wide_support;
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_prism::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use wide_support::{Ctx, SRGB};

// ---------------------------------------------------------------- counting allocator (allocations, per thread)
thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}
fn allocations() -> u64 {
    ALLOCS.with(Cell::get)
}
fn count_one() {
    let _ = ALLOCS.try_with(|c| c.set(c.get().saturating_add(1)));
}
struct Counting;
// SAFETY: every method forwards to the System allocator with the arguments it received; the only addition is a
// thread-local counter update that never allocates (try_with, const-initialised cell).
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_one();
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count_one();
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count_one();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}
#[global_allocator]
static GLOBAL: Counting = Counting;

const P3: &[u8] = include_bytes!("profiles/DisplayP3-v4.icc");
const CMYK: &[u8] = include_bytes!("oracle/profiles/hsk-synth-cmyk-v2.icc");

fn id(n: u32) -> DomainId {
    DomainId::parse(&format!("SCPF-019abcde-0000-7000-8000-{n:012x}")).unwrap()
}
fn input<'a>(id: &'a DomainId, bytes: &'a [u8]) -> ProfileInput<'a> {
    ProfileInput { profile_id: id, bytes, expected_sha256: profile_hash(bytes) }
}

#[test]
fn not_proven_parity_lines() {
    println!("NOT_PROVEN: soft-proof output equals Photoshop View > Proof Colors (no proof case in the D5 Photoshop corpus)");
    println!("NOT_PROVEN: gamut warning equals Photoshop View > Gamut Warning and LittleCMS cmsCreateProofingTransform gamut check (no oracle file)");
}

#[test]
fn proof_condition_has_six_independent_fields() {
    let (cid, cmyk) = (id(1), CMYK);
    let base = ProofCondition::working_cmyk(input(&cid, cmyk));
    let r0 = base.record();
    assert_eq!(r0.kind, ProofProfileKind::Icc);
    assert_eq!(r0.profile_sha256, Some(profile_hash(CMYK)));
    // Each mutation changes exactly one of the six fields of the record.
    let (oid, srgb) = (id(2), SRGB);
    for i in 0..6 {
        let mut c = base.clone();
        match i {
            0 => c.proof_profile = ProofProfile::Icc(input(&oid, srgb)),
            1 => c.proof_intent = Intent::Perceptual,
            2 => c.display_intent = Intent::Saturation,
            3 => c.black_point_compensation = !c.black_point_compensation,
            4 => c.simulate_paper_color = !c.simulate_paper_color,
            _ => c.simulate_black_ink = !c.simulate_black_ink,
        }
        let r = c.record();
        let changed = [
            r.profile_sha256 != r0.profile_sha256,
            r.proof_intent != r0.proof_intent,
            r.display_intent != r0.display_intent,
            r.black_point_compensation != r0.black_point_compensation,
            r.simulate_paper_color != r0.simulate_paper_color,
            r.simulate_black_ink != r0.simulate_black_ink,
        ];
        let expected: Vec<bool> = (0..6).map(|j| j == i).collect();
        assert_eq!(changed.to_vec(), expected, "mutation {i} touched another field");
    }
    // Paper colour and black ink are separate booleans: all four combinations are representable.
    for (paper, ink) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut c = base.clone();
        c.simulate_paper_color = paper;
        c.simulate_black_ink = ink;
        let r = c.record();
        assert_eq!((r.simulate_paper_color, r.simulate_black_ink), (paper, ink));
        let json = r.to_json();
        assert!(json.contains(&format!("\"simulate_paper_color\":{paper}")) && json.contains(&format!("\"simulate_black_ink\":{ink}")));
    }
    // The two intents are separate fields of the saved record.
    let mut c = base.clone();
    c.proof_intent = Intent::Perceptual;
    c.display_intent = Intent::AbsoluteColorimetric;
    let json = c.record().to_json();
    assert!(json.contains("\"proof_intent\":\"perceptual\"") && json.contains("\"display_intent\":\"absolute_colorimetric\""), "{json}");
    // Names are validated and escaped.
    assert_eq!(ProofCondition::new("", ProofProfile::LegacyUncompensatedRgb).validate(), Err(ProofError::NameEmpty));
    assert_eq!(ProofCondition::new(&"x".repeat(MAX_PROOF_NAME_BYTES + 1), ProofProfile::LegacyUncompensatedRgb).validate(), Err(ProofError::NameTooLong));
    assert_eq!(ProofCondition::new("a\u{7}b", ProofProfile::LegacyUncompensatedRgb).validate(), Err(ProofError::NameControl));
    let quoted = ProofCondition::new("say \"hi\" \\", ProofProfile::LegacyUncompensatedRgb).record().to_json();
    assert!(quoted.starts_with("{\"name\":\"say \\\"hi\\\" \\\\\""), "{quoted}");
}

#[test]
fn presets_include_the_spec_minimum() {
    let (cid, rid) = (id(1), id(2));
    let presets = shipped_proof_conditions(input(&cid, CMYK), input(&rid, SRGB));
    let names: Vec<&str> = presets.iter().map(|c| c.name.as_str()).collect();
    for required in [PRESET_WORKING_CMYK, PRESET_WORKING_RGB, PRESET_LEGACY_UNCOMPENSATED_RGB, PRESET_PROTANOPIA, PRESET_DEUTERANOPIA] {
        assert!(names.contains(&required), "missing preset {required}: {names:?}");
    }
    for c in &presets {
        c.validate().unwrap();
    }
    let cmyk = select_proof_condition(&presets, PRESET_WORKING_CMYK).unwrap();
    assert_eq!(cmyk.record().profile_sha256, Some(profile_hash(CMYK)));
    let rgb = select_proof_condition(&presets, PRESET_WORKING_RGB).unwrap();
    assert_eq!(rgb.record().profile_sha256, Some(profile_hash(SRGB)));
    assert_eq!(
        select_proof_condition(&presets, PRESET_LEGACY_UNCOMPENSATED_RGB).unwrap().proof_profile.kind(),
        ProofProfileKind::LegacyUncompensatedRgb
    );
    // The accessibility conditions are first-class proof profiles, not a filter.
    assert_eq!(
        select_proof_condition(&presets, PRESET_PROTANOPIA).unwrap().proof_profile.kind(),
        ProofProfileKind::ColourVision(ColourVision::Protanopia)
    );
    assert_eq!(
        select_proof_condition(&presets, PRESET_DEUTERANOPIA).unwrap().proof_profile.kind(),
        ProofProfileKind::ColourVision(ColourVision::Deuteranopia)
    );
    assert!(select_proof_condition(&presets, "no_such_condition").is_none());
}

// ---------------------------------------------------------------- slice 2: the proof chain
/// Deterministic document RGB pixels spread over the cube (0..1).
fn rgb_pixels(n: usize) -> Vec<f32> {
    (0..n)
        .flat_map(|i| {
            let f = |k: usize, m: usize| ((i.wrapping_mul(k) % m) as f32) / ((m - 1) as f32);
            [f(7, 17), f(11, 13), f(5, 19)]
        })
        .collect()
}
fn wide(src: &[u8], dst: &[u8], pixels: &[f32], intent: Intent, bpc: bool) -> Vec<f32> {
    let ctx = Ctx::new();
    wide_support::run(&mut wide_support::engine(&ctx), &ctx, src, dst, pixels, intent, bpc).unwrap().pixels
}
fn cancel() -> CancellationToken {
    CancellationToken::default()
}

#[test]
fn hostile_proof_profiles_are_refused_before_allocation() {
    let (did, pid, sid) = (id(10), id(11), id(12));
    let refused = |bytes: &[u8], sha: [u8; 32], profile_id: &DomainId| {
        let cond = ProofCondition::new("hostile", ProofProfile::Icc(ProfileInput { profile_id, bytes, expected_sha256: sha }));
        let doc = input(&did, SRGB);
        let display = input(&sid, SRGB);
        let token = cancel();
        let before = allocations();
        let r = prepare_soft_proof(doc, &cond, display, &token).unwrap_err();
        (r, allocations() - before)
    };
    // Size and hash are checked first, from the borrowed bytes: nothing is allocated before the refusal.
    let oversize = vec![0u8; MAX_WIDE_PROFILE_BYTES + 1];
    assert_eq!(refused(&oversize, profile_hash(&oversize), &pid), (ProofError::Wide(WideError::ProfileLimit), 0));
    assert_eq!(refused(CMYK, [0u8; 32], &pid), (ProofError::Wide(WideError::HashMismatch), 0));
    let wrong_kind = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000011").unwrap();
    assert_eq!(refused(CMYK, profile_hash(CMYK), &wrong_kind), (ProofError::Wide(WideError::InvalidProfileId), 0));
    // Framing refusals (after the size/hash gate).
    let truncated = &CMYK[..200];
    assert_eq!(refused(truncated, profile_hash(truncated), &pid).0, ProofError::Wide(WideError::MalformedProfile));
    let mut huge_count = SRGB.to_vec();
    huge_count[128..132].copy_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(refused(&huge_count, profile_hash(&huge_count), &pid).0, ProofError::Wide(WideError::MalformedProfile));
    let mut link = SRGB.to_vec();
    link[12..16].copy_from_slice(b"link");
    assert_eq!(refused(&link, profile_hash(&link), &pid).0, ProofError::Wide(WideError::UnsupportedClass));
    // The proof profile is admitted before the document: a hostile proof wins over a hostile document.
    let cond = ProofCondition::new(
        "hostile",
        ProofProfile::Icc(ProfileInput { profile_id: &pid, bytes: &oversize, expected_sha256: profile_hash(&oversize) }),
    );
    let bad_doc = ProfileInput { profile_id: &did, bytes: SRGB, expected_sha256: [1u8; 32] };
    assert_eq!(
        prepare_soft_proof(bad_doc, &cond, input(&sid, SRGB), &cancel()).unwrap_err(),
        ProofError::Wide(WideError::ProfileLimit)
    );
    // A CMYK display is refused; a CMYK document cannot use legacy uncompensated RGB.
    let ok = ProofCondition::working_cmyk(input(&pid, CMYK));
    assert_eq!(prepare_soft_proof(input(&did, SRGB), &ok, input(&sid, CMYK), &cancel()).unwrap_err(), ProofError::DisplayNotRgb);
    let legacy = ProofCondition::legacy_uncompensated_rgb();
    assert_eq!(
        prepare_soft_proof(input(&did, CMYK), &legacy, input(&sid, SRGB), &cancel()).unwrap_err(),
        ProofError::LegacyNeedsRgbDocument
    );
    // Hostile pixel buffers and geometry (V4-02): refused before the output is allocated.
    let proof = prepare_soft_proof(input(&did, SRGB), &ok, input(&sid, SRGB), &cancel()).unwrap();
    assert_eq!(proof.run(&[0.5; 4], 1, true, &cancel()).unwrap_err(), ProofError::Wide(WideError::ChannelCount));
    assert_eq!(proof.run(&[], 1, true, &cancel()).unwrap_err(), ProofError::Wide(WideError::ChannelCount));
    assert_eq!(proof.run(&[0.5; 6], usize::MAX, true, &cancel()).unwrap_err(), ProofError::Geometry);
    assert_eq!(proof.run(&[0.5; 6], 0, true, &cancel()).unwrap_err(), ProofError::Geometry);
    assert_eq!(proof.run(&[0.5, f32::NAN, 0.5], 1, true, &cancel()).unwrap_err(), ProofError::Wide(WideError::NonfiniteChannel));
    assert_eq!(proof.run(&[0.5, 1.5, 0.5], 1, true, &cancel()).unwrap_err(), ProofError::Wide(WideError::ChannelRange));
    let over = vec![0.5f32; (MAX_WIDE_PIXELS + 1) * 3];
    let token = cancel();
    let before = allocations();
    let r = proof.run(&over, 1, true, &token);
    let allocated = allocations() - before;
    assert_eq!(r.unwrap_err(), ProofError::Wide(WideError::PixelLimit));
    assert_eq!(allocated, 0, "pixel_limit refused before allocating");
    let canceled = cancel();
    canceled.cancel();
    assert_eq!(proof.run(&[0.5; 3], 1, true, &canceled).unwrap_err(), ProofError::Wide(WideError::Canceled));
}

#[test]
fn soft_proof_never_changes_document_values() {
    let (did, pid, sid) = (id(10), id(11), id(12));
    let doc = rgb_pixels(300);
    let copy = doc.clone();
    for cond in [
        ProofCondition::working_cmyk(input(&pid, CMYK)),
        ProofCondition::colour_vision(ColourVision::Deuteranopia),
        ProofCondition::legacy_uncompensated_rgb(),
    ] {
        let proof = prepare_soft_proof(input(&did, P3), &cond, input(&sid, SRGB), &cancel()).unwrap();
        for enabled in [true, false] {
            let preview = proof.run(&doc, 20, enabled, &cancel()).unwrap();
            assert_eq!(preview.receipt.enabled, enabled);
            assert_eq!((preview.receipt.width, preview.receipt.height, preview.receipt.pixel_count), (20, 15, 300));
            assert_eq!(preview.pixels.len(), doc.len());
        }
        assert!(doc.iter().zip(&copy).all(|(a, b)| a.to_bits() == b.to_bits()), "{}: document values changed", cond.name);
    }
    // The proof shows something other than the plain display transform (it simulates; it does not pass through).
    let cond = ProofCondition::working_cmyk(input(&pid, CMYK));
    let proof = prepare_soft_proof(input(&did, P3), &cond, input(&sid, SRGB), &cancel()).unwrap();
    let on = proof.run(&doc, 20, true, &cancel()).unwrap();
    let off = proof.run(&doc, 20, false, &cancel()).unwrap();
    assert_ne!(on.receipt.output_sha256, off.receipt.output_sha256);
    // Legacy uncompensated RGB shows the document values unmanaged.
    let legacy = ProofCondition::legacy_uncompensated_rgb();
    let legacy = prepare_soft_proof(input(&did, P3), &legacy, input(&sid, SRGB), &cancel()).unwrap();
    let shown = legacy.run(&doc, 20, true, &cancel()).unwrap();
    assert!(shown.receipt.unmanaged && shown.receipt.display_stage.is_none());
    assert_eq!(shown.pixels, doc);
}

#[test]
fn toggle_off_equals_the_plain_display_transform() {
    let (did, pid, sid) = (id(10), id(11), id(12));
    let doc = rgb_pixels(500);
    for display_intent in [Intent::RelativeColorimetric, Intent::Perceptual] {
        for mut cond in [ProofCondition::working_cmyk(input(&pid, CMYK)), ProofCondition::colour_vision(ColourVision::Protanopia)] {
            cond.display_intent = display_intent;
            cond.simulate_paper_color = true;
            let proof = prepare_soft_proof(input(&did, P3), &cond, input(&sid, SRGB), &cancel()).unwrap();
            let off = proof.run(&doc, 25, false, &cancel()).unwrap();
            assert_eq!(off.pixels, wide(P3, SRGB, &doc, display_intent, true), "{}", cond.name);
            assert!(!off.receipt.enabled && off.receipt.display_stage.is_none());
        }
    }
}

#[test]
fn two_intents_are_applied_separately() {
    let (did, pid, sid) = (id(10), id(11), id(12));
    let doc = rgb_pixels(400);
    let (p, r) = (Intent::Perceptual, Intent::RelativeColorimetric);
    let mut digests = Vec::new();
    for (proof_intent, display_intent) in [(p, r), (r, p), (p, p), (r, r)] {
        let mut cond = ProofCondition::working_cmyk(input(&pid, CMYK));
        cond.proof_intent = proof_intent;
        cond.display_intent = display_intent;
        let proof = prepare_soft_proof(input(&did, SRGB), &cond, input(&sid, SRGB), &cancel()).unwrap();
        let preview = proof.run(&doc, 20, true, &cancel()).unwrap();
        // Document -> proof device under proof_intent (with the condition's BPC), then proof device -> display
        // under display_intent: the composition of two separate wide-path transforms, bit for bit.
        let cmyk = wide(SRGB, CMYK, &doc, proof_intent, true);
        let expected = wide(CMYK, SRGB, &cmyk, display_intent, true);
        assert_eq!(preview.pixels, expected, "proof {proof_intent:?} display {display_intent:?}");
        let receipt = &preview.receipt;
        assert_eq!((receipt.condition.proof_intent, receipt.condition.display_intent), (proof_intent, display_intent));
        assert_eq!(receipt.display_stage.unwrap().intent, display_intent);
        assert!(receipt.proof_bpc.requested);
        digests.push(receipt.output_sha256);
    }
    // The synthetic CMYK profile has distinct tables per intent: swapping the two intents changes the proof.
    assert_ne!(digests[0], digests[1]);
    assert_ne!(digests[2], digests[3]);
}

#[test]
fn paper_and_black_ink_flags_are_independent() {
    let (did, pid, sid) = (id(10), id(11), id(12));
    let mut doc = rgb_pixels(198);
    doc.extend_from_slice(&[1.0, 1.0, 1.0, 0.0, 0.0, 0.0]); // white then black, the last two pixels
    let n = doc.len();
    let mut previews = Vec::new();
    for (paper, ink) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut cond = ProofCondition::working_cmyk(input(&pid, CMYK));
        cond.simulate_paper_color = paper;
        cond.simulate_black_ink = ink;
        let proof = prepare_soft_proof(input(&did, SRGB), &cond, input(&sid, SRGB), &cancel()).unwrap();
        let preview = proof.run(&doc, 1, true, &cancel()).unwrap();
        let stage = preview.receipt.display_stage.unwrap();
        assert_eq!(stage.paper_white_scale.is_some(), paper);
        assert_eq!(stage.intent == Intent::AbsoluteColorimetric, paper);
        assert_eq!(stage.bpc.requested, !ink);
        if ink {
            assert_eq!(stage.bpc.rule, DISPLAY_BPC_RULE_BLACK_INK_SIMULATED);
        }
        previews.push(preview);
    }
    for i in 0..4 {
        for j in i + 1..4 {
            assert_ne!(previews[i].receipt.output_sha256, previews[j].receipt.output_sha256, "combinations {i} and {j} agree");
        }
    }
    let cmyk = wide(SRGB, CMYK, &doc, Intent::RelativeColorimetric, true);
    // Neither = relative with black point compensation on the display stage.
    assert_eq!(previews[0].pixels, wide(CMYK, SRGB, &cmyk, Intent::RelativeColorimetric, true));
    // Black ink alone = proof device -> display without black point compensation.
    assert_eq!(previews[2].pixels, wide(CMYK, SRGB, &cmyk, Intent::RelativeColorimetric, false));
    // Paper and black ink = absolute colorimetric proof device -> display.
    assert_eq!(previews[3].pixels, wide(CMYK, SRGB, &cmyk, Intent::AbsoluteColorimetric, false));
    let sum = |p: &ProofPreview, at: usize| p.pixels[at..at + 3].iter().sum::<f32>();
    let (white, black) = (n - 6, n - 3);
    // Paper colour darkens the paper white (wtpt 0.87/0.90/0.69 against D50), with or without black ink.
    assert!(sum(&previews[1], white) + 0.05 < sum(&previews[0], white));
    assert!(sum(&previews[3], white) + 0.05 < sum(&previews[2], white));
    // Black ink lifts the black, with or without paper colour.
    assert!(sum(&previews[2], black) > sum(&previews[0], black) + 0.03);
    assert!(sum(&previews[3], black) > sum(&previews[1], black) + 0.03);
}

#[test]
fn proof_is_deterministic_across_threads() {
    let (did, pid, sid) = (id(10), id(11), id(12));
    let doc = rgb_pixels(4096);
    let cond = ProofCondition::working_cmyk(input(&pid, CMYK));
    let proof = prepare_soft_proof(input(&did, P3), &cond, input(&sid, SRGB), &cancel()).unwrap();
    let serial = proof.run(&doc, 64, true, &cancel()).unwrap();
    let results: Vec<ProofPreview> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..4).map(|_| s.spawn(|| proof.run(&doc, 64, true, &cancel()).unwrap())).collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for r in results {
        assert_eq!(r.receipt.output_sha256, serial.receipt.output_sha256);
        assert!(r.pixels.iter().zip(&serial.pixels).all(|(a, b)| a.to_bits() == b.to_bits()));
    }
}

// ---------------------------------------------------------------- slice 3: colour-vision conditions
fn mat_mul(m: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    m.map(|r| r[0] * v[0] + r[1] * v[1] + r[2] * v[2])
}

#[test]
fn cvd_conditions_run_through_the_proof_chain() {
    let (did, sid) = (id(10), id(12));
    let mut doc = rgb_pixels(240);
    doc.extend_from_slice(&[1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
    let mut digests = Vec::new();
    for kind in [ColourVision::Protanopia, ColourVision::Deuteranopia, ColourVision::Tritanopia] {
        let cond = ProofCondition::colour_vision(kind);
        let proof = prepare_soft_proof(input(&did, SRGB), &cond, input(&sid, SRGB), &cancel()).unwrap();
        assert_eq!(proof.proof_space(), Some(DeviceSpace::Rgb));
        let preview = proof.run(&doc, 1, true, &cancel()).unwrap();
        // Same pipeline, toggle and inspection state as an ICC proof: the receipt names the linear-light sRGB proof
        // device, the published matrix and the display stage.
        let receipt = &preview.receipt;
        assert_eq!(receipt.condition.kind, ProofProfileKind::ColourVision(kind));
        assert_eq!(receipt.proof_profile_id.as_ref().map(|i| i.as_str()), Some(CVD_PROOF_DEVICE_PROFILE_ID));
        assert_eq!(receipt.proof_profile_sha256, Some(profile_hash(WORKING_PROFILE_LINEAR_SRGB)));
        assert_eq!(receipt.simulation_matrix, Some(kind.matrix()));
        assert!(receipt.display_stage.is_some() && receipt.enabled);
        // Analytic expectation: sRGB decode, clamp, Machado matrix on linear light, clamp, sRGB encode.
        for (px, out) in doc.chunks_exact(3).zip(preview.pixels.chunks_exact(3)) {
            let lin = [0, 1, 2].map(|c| wide_support::srgb_decode(f64::from(px[c])));
            let sim = mat_mul(&kind.matrix(), lin).map(|v| wide_support::srgb_encode(v.clamp(0.0, 1.0)));
            for c in 0..3 {
                assert!((f64::from(out[c]) - sim[c]).abs() < 2e-3, "{kind:?} {px:?}: {out:?} vs {sim:?}");
            }
        }
        digests.push(receipt.output_sha256);
        // Toggled off it is the ordinary display transform, like every proof condition.
        let off = proof.run(&doc, 1, false, &cancel()).unwrap();
        assert_eq!(off.pixels, wide(SRGB, SRGB, &doc, Intent::RelativeColorimetric, true));
    }
    assert_ne!(digests[0], digests[1]);
    assert_ne!(digests[1], digests[2]);
    // Protanopia folds red and green onto one hue: pure red and pure green land on the same blue-free axis.
    let cond = ProofCondition::colour_vision(ColourVision::Protanopia);
    let proof = prepare_soft_proof(input(&did, SRGB), &cond, input(&sid, SRGB), &cancel()).unwrap();
    let rg = proof.run(&[1.0, 0.0, 0.0, 0.0, 1.0, 0.0], 1, true, &cancel()).unwrap().pixels;
    assert!(rg[2] < 0.05 && rg[5] < 0.05, "{rg:?}");
}

#[test]
fn cvd_neutral_axis_is_preserved() {
    for m in [MACHADO_2009_PROTANOPIA, MACHADO_2009_DEUTERANOPIA, MACHADO_2009_TRITANOPIA] {
        for row in m {
            assert!((row.iter().sum::<f64>() - 1.0).abs() < 2e-6, "{row:?}");
        }
    }
    let (did, sid) = (id(10), id(12));
    let grays: Vec<f32> = (0..=20).flat_map(|i| [i as f32 / 20.0; 3]).collect();
    for kind in [ColourVision::Protanopia, ColourVision::Deuteranopia, ColourVision::Tritanopia] {
        let cond = ProofCondition::colour_vision(kind);
        let proof = prepare_soft_proof(input(&did, SRGB), &cond, input(&sid, SRGB), &cancel()).unwrap();
        let on = proof.run(&grays, 21, true, &cancel()).unwrap().pixels;
        let off = proof.run(&grays, 21, false, &cancel()).unwrap().pixels;
        for (a, b) in on.chunks_exact(3).zip(off.chunks_exact(3)) {
            assert!((a[0] - a[1]).abs() < 1e-4 && (a[1] - a[2]).abs() < 1e-4, "{kind:?} gray not neutral: {a:?}");
            assert!((a[0] - b[0]).abs() < 1e-3, "{kind:?} gray moved: {a:?} vs {b:?}");
        }
    }
}

#[test]
fn operator_condition_bounds_are_enforced() {
    let ok_row = [1.0, 0.0, 0.0];
    let with = |row: [f64; 3]| OperatorSimulation::new([row, [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
    assert_eq!(with([f64::NAN, 0.0, 0.0]), Err(ProofError::OperatorMatrixNonfinite));
    assert_eq!(with([f64::INFINITY, -1.0, 0.0]), Err(ProofError::OperatorMatrixNonfinite));
    assert_eq!(with([4.5, -3.0, -0.5]), Err(ProofError::OperatorCoefficientRange));
    assert_eq!(with([-4.0001, 2.5, 2.5]), Err(ProofError::OperatorCoefficientRange));
    assert_eq!(with([-1.0, 0.25, 0.25]), Err(ProofError::OperatorRowSumRange));
    assert_eq!(with([1.0, 1.0, 0.5]), Err(ProofError::OperatorRowSumRange));
    // The limits themselves are admitted.
    assert!(with([OPERATOR_COEFFICIENT_LIMIT, -2.0, -1.0]).is_ok());
    assert!(with([OPERATOR_ROW_SUM_MAX, 0.0, 0.0]).is_ok());
    assert!(with([OPERATOR_ROW_SUM_MIN, 0.0, 0.0]).is_ok());
    assert_eq!(with(ok_row).unwrap().matrix()[0], ok_row);
    // An operator condition runs the same pipeline: the Machado protanopia matrix supplied by the operator gives
    // the shipped protanopia proof bit for bit, and the identity leaves an in-gamut document as displayed.
    let (did, sid) = (id(10), id(12));
    let doc = rgb_pixels(200);
    let shipped = ProofCondition::colour_vision(ColourVision::Protanopia);
    let operator = ProofCondition::operator("my protan", OperatorSimulation::new(MACHADO_2009_PROTANOPIA).unwrap());
    let run = |cond: &ProofCondition, enabled: bool| {
        prepare_soft_proof(input(&did, SRGB), cond, input(&sid, SRGB), &cancel()).unwrap().run(&doc, 10, enabled, &cancel()).unwrap()
    };
    let a = run(&shipped, true);
    let b = run(&operator, true);
    assert_eq!(a.pixels, b.pixels);
    assert_eq!(b.receipt.condition.kind, ProofProfileKind::OperatorSimulation);
    assert!(b.receipt.condition.to_json().contains("\"operator_matrix\":[[0.152286,"));
    let identity = ProofCondition::operator("identity", OperatorSimulation::new([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]).unwrap());
    let on = run(&identity, true);
    let off = run(&identity, false);
    assert!(on.pixels.iter().zip(&off.pixels).all(|(x, y)| (x - y).abs() < 1e-4));
}

// ---------------------------------------------------------------- slice 4: gamut warning and in-gamut substitute
fn wide_receipt(src: &[u8], dst: &[u8], pixels: &[f32], intent: Intent, bpc: bool) -> WideReceipt {
    let ctx = Ctx::new();
    wide_support::run(&mut wide_support::engine(&ctx), &ctx, src, dst, pixels, intent, bpc).unwrap().receipt
}

#[test]
fn gamut_mask_marks_exactly_the_clipped_pixels() {
    let (did, pid, sid) = (id(10), id(11), id(12));
    let mut doc = rgb_pixels(300);
    doc.extend_from_slice(&[0.0, 1.0, 0.0, 1.0, 0.0, 0.0, 0.5, 0.5, 0.5]);
    let count = doc.len() / 3;
    let style = GamutWarningStyle::default();
    for proof_intent in [Intent::RelativeColorimetric, Intent::Perceptual, Intent::Saturation] {
        let mut cond = ProofCondition::working_rgb(input(&pid, SRGB));
        cond.proof_intent = proof_intent;
        let proof = prepare_soft_proof(input(&did, P3), &cond, input(&sid, SRGB), &cancel()).unwrap();
        let warning = proof.gamut_warning(&doc, 1, style, &cancel()).unwrap();
        assert_eq!(warning.method, GamutMethod::LinearDevice);
        // STU-COL-138: perceptual and saturation do not clip, so the check runs under relative colorimetric.
        assert_eq!(warning.gamut_intent, Some(Intent::RelativeColorimetric));
        assert_eq!(warning.threshold_delta_e76, None);
        // Pixel for pixel, the mask is the wide path's clipping report (STU-COL-138) of the same conversion.
        let mut expected = 0u64;
        for (i, px) in doc.chunks_exact(3).enumerate() {
            let clip = wide_receipt(P3, SRGB, px, Intent::RelativeColorimetric, true).clip;
            assert_eq!(clip.method, ClipMethod::LinearDevice);
            assert_eq!(warning.mask[i], clip.pixels_clipped == 1, "pixel {i} {px:?}");
            expected += clip.pixels_clipped;
        }
        assert_eq!(warning.out_of_gamut_pixels, expected);
        assert_eq!(wide_receipt(P3, SRGB, &doc, Intent::RelativeColorimetric, true).clip.pixels_clipped, expected);
        assert!(warning.mask[count - 3] && warning.mask[count - 2] && !warning.mask[count - 1]);
        // The proof run reports the same clipping for a colorimetric proof intent.
        if proof_intent == Intent::RelativeColorimetric {
            assert_eq!(proof.run(&doc, 1, true, &cancel()).unwrap().receipt.proof_clip.pixels_clipped, expected);
        }
    }
    // Legacy uncompensated RGB has no proof device: nothing is marked.
    let legacy = ProofCondition::legacy_uncompensated_rgb();
    let legacy = prepare_soft_proof(input(&did, P3), &legacy, input(&sid, SRGB), &cancel()).unwrap();
    let none = legacy.gamut_warning(&doc, 1, style, &cancel()).unwrap();
    assert_eq!((none.method, none.out_of_gamut_pixels, none.area_fraction), (GamutMethod::NotApplicable, 0, 0.0));
}

#[test]
fn table_destination_gamut_check_uses_round_trip() {
    let (did, pid, sid) = (id(10), id(11), id(12));
    // In-gamut document colours made from CMYK device values of the proof profile itself.
    let inks: Vec<f32> = vec![
        0.2, 0.1, 0.1, 0.0, 0.1, 0.3, 0.2, 0.1, 0.3, 0.2, 0.4, 0.0, 0.0, 0.0, 0.0, 0.5, 0.15, 0.15, 0.15, 0.15, 0.05, 0.05,
        0.05, 0.0,
    ];
    let inside: Vec<f32> = wide(CMYK, SRGB, &inks, Intent::RelativeColorimetric, false).iter().map(|v| v.clamp(0.0, 1.0)).collect();
    let n_in = inside.len() / 3;
    let mut doc = inside.clone();
    doc.extend_from_slice(&[0.0, 0.0, 1.0, 0.0, 1.0, 0.0]); // sRGB blue and green: beyond the ink gamut
    let mut cond = ProofCondition::working_cmyk(input(&pid, CMYK));
    cond.black_point_compensation = false;
    let proof = prepare_soft_proof(input(&did, SRGB), &cond, input(&sid, SRGB), &cancel()).unwrap();
    let warning = proof.gamut_warning(&doc, 1, GamutWarningStyle::default(), &cancel()).unwrap();
    // A table destination cannot reveal clipping from its clamped output: the wide clip report says so, and the
    // warning uses the PCS round trip with the declared threshold instead.
    assert_eq!(wide_receipt(SRGB, CMYK, &doc, Intent::RelativeColorimetric, false).clip.method, ClipMethod::Unavailable);
    assert_eq!(warning.method, GamutMethod::RoundTrip);
    assert_eq!(warning.threshold_delta_e76, Some(GAMUT_ROUND_TRIP_DELTA_E76));
    assert!(warning.mask[..n_in].iter().all(|m| !m), "in-gamut colours marked: {:?}", &warning.mask[..n_in]);
    assert!(warning.mask[n_in] && warning.mask[n_in + 1], "out-of-gamut colours unmarked: {:?}", warning.mask);
    assert!(warning.max_delta_e76.unwrap() > 2.0 * GAMUT_ROUND_TRIP_DELTA_E76);
    // Perceptual proof intent: checked under relative colorimetric, same mask.
    cond.proof_intent = Intent::Perceptual;
    let perceptual = prepare_soft_proof(input(&did, SRGB), &cond, input(&sid, SRGB), &cancel()).unwrap();
    let pw = perceptual.gamut_warning(&doc, 1, GamutWarningStyle::default(), &cancel()).unwrap();
    assert_eq!(pw.gamut_intent, Some(Intent::RelativeColorimetric));
    assert_eq!(pw.mask, warning.mask);
    // Colour-vision conditions use the linear-light sRGB proof device: the linear method.
    let cvd = ProofCondition::colour_vision(ColourVision::Protanopia);
    let cvd = prepare_soft_proof(input(&did, P3), &cvd, input(&sid, SRGB), &cancel()).unwrap();
    assert_eq!(cvd.gamut_warning(&[0.0, 1.0, 0.0], 1, GamutWarningStyle::default(), &cancel()).unwrap().method, GamutMethod::LinearDevice);
}

#[test]
fn gamut_opacity_bounds_follow_the_parameter_table() {
    // Parameter table row: gamut_warning.opacity, 0..100, default 100, integer percent.
    let default = GamutWarningStyle::default();
    assert_eq!(default.opacity_percent(), GAMUT_WARNING_OPACITY_DEFAULT);
    assert_eq!((GAMUT_WARNING_OPACITY_MIN, GAMUT_WARNING_OPACITY_MAX, GAMUT_WARNING_OPACITY_DEFAULT), (0, 100, 100));
    assert_eq!(default.colour(), DEFAULT_GAMUT_WARNING_COLOUR);
    let colour = [1.0, 0.0, 1.0];
    assert!(GamutWarningStyle::new(colour, 0).is_ok());
    assert!(GamutWarningStyle::new(colour, 100).is_ok());
    assert_eq!(GamutWarningStyle::new(colour, 101), Err(ProofError::OpacityRange));
    assert_eq!(GamutWarningStyle::new(colour, u32::MAX), Err(ProofError::OpacityRange));
    assert_eq!(GamutWarningStyle::new([f32::NAN, 0.0, 0.0], 50), Err(ProofError::WarningColourRange));
    assert_eq!(GamutWarningStyle::new([1.5, 0.0, 0.0], 50), Err(ProofError::WarningColourRange));
    assert_eq!(GamutWarningStyle::new([0.0, -0.1, 0.0], 50), Err(ProofError::WarningColourRange));
    // The overlay blends the warning colour at the configured opacity over marked pixels only.
    let (did, pid, sid) = (id(10), id(11), id(12));
    let doc = [0.0, 1.0, 0.0, 0.5, 0.5, 0.5];
    let cond = ProofCondition::working_rgb(input(&pid, SRGB));
    let proof = prepare_soft_proof(input(&did, P3), &cond, input(&sid, SRGB), &cancel()).unwrap();
    let preview = proof.run(&doc, 2, true, &cancel()).unwrap().pixels;
    for opacity in [0u32, 50, 100] {
        let style = GamutWarningStyle::new(colour, opacity).unwrap();
        let warning = proof.gamut_warning(&doc, 2, style, &cancel()).unwrap();
        assert_eq!(warning.mask, vec![true, false]);
        assert_eq!(warning.style.opacity_percent(), opacity);
        let shown = warning.apply_overlay(&preview).unwrap();
        let a = opacity as f32 / 100.0;
        for c in 0..3 {
            assert!((shown[c] - ((1.0 - a) * preview[c] + a * colour[c])).abs() < 1e-6);
        }
        assert_eq!(&shown[3..], &preview[3..]);
        assert!(warning.apply_overlay(&preview[..3]).is_err());
    }
}

#[test]
fn area_fraction_and_regions_are_bounded() {
    let (did, pid, sid) = (id(10), id(11), id(12));
    let cond = ProofCondition::working_rgb(input(&pid, SRGB));
    let proof = prepare_soft_proof(input(&did, P3), &cond, input(&sid, SRGB), &cancel()).unwrap();
    let style = GamutWarningStyle::default();
    let (w, h) = (64usize, 64usize);
    let grey = [0.5f32, 0.5, 0.5];
    let green = [0.0f32, 1.0, 0.0];
    // All in gamut.
    let plain: Vec<f32> = (0..w * h).flat_map(|_| grey).collect();
    let none = proof.gamut_warning(&plain, w, style, &cancel()).unwrap();
    assert_eq!((none.out_of_gamut_pixels, none.area_fraction, none.region_count, none.bounds), (0, 0.0, 0, None));
    assert!(none.regions.is_empty() && !none.regions_truncated);
    // One 4x3 block: one region with its exact box.
    let mut block = plain.clone();
    for y in 10..13 {
        for x in 20..24 {
            block[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&green);
        }
    }
    let one = proof.gamut_warning(&block, w, style, &cancel()).unwrap();
    let region = GamutRegion { x0: 20, y0: 10, x1: 23, y1: 12, pixels: 12 };
    assert_eq!((one.out_of_gamut_pixels, one.region_count, one.regions.clone(), one.bounds), (12, 1, vec![region], Some(region)));
    assert_eq!(one.area_fraction, 12.0 / 4096.0);
    assert_eq!((one.width, one.height), (w, h));
    // 484 isolated pixels (every third column and row): more components than the cap lists.
    let mut dots = plain.clone();
    let mut placed = 0u64;
    for y in (0..h).step_by(3) {
        for x in (0..w).step_by(3) {
            dots[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&green);
            placed += 1;
        }
    }
    let many = proof.gamut_warning(&dots, w, style, &cancel()).unwrap();
    assert_eq!(placed, 484);
    assert_eq!(many.out_of_gamut_pixels, placed);
    assert_eq!(many.region_count, placed);
    assert_eq!(many.regions.len(), MAX_GAMUT_REGIONS);
    assert!(many.regions_truncated);
    assert!(many.regions.iter().all(|r| r.pixels == 1 && r.x0 == r.x1 && r.y0 == r.y1));
    assert_eq!(many.area_fraction, placed as f64 / 4096.0);
    assert_eq!(many.bounds, Some(GamutRegion { x0: 0, y0: 0, x1: 63, y1: 63, pixels: placed }));
    // Geometry is checked before any work: a width that does not divide the pixel count is refused.
    assert_eq!(proof.gamut_warning(&dots, 63, style, &cancel()).unwrap_err(), ProofError::Geometry);
}

#[test]
fn substitute_is_in_gamut_and_reports_delta() {
    let (did, pid, sid) = (id(10), id(11), id(12));
    let mut cond = ProofCondition::working_rgb(input(&pid, SRGB));
    cond.black_point_compensation = false;
    let proof = prepare_soft_proof(input(&did, P3), &cond, input(&sid, SRGB), &cancel()).unwrap();
    // In gamut: nothing to substitute.
    let grey = proof.in_gamut_substitute(&[0.5, 0.4, 0.3], &cancel()).unwrap();
    assert!(!grey.out_of_gamut && grey.substitute == grey.original && grey.delta_e76 == 0.0);
    // Out of gamut: P3 green. The substitute is the sRGB-clipped colour expressed in P3, in gamut, with its delta.
    let green = [0.0f32, 1.0, 0.0];
    let s = proof.in_gamut_substitute(&green, &cancel()).unwrap();
    assert!(s.out_of_gamut && s.substitute_in_gamut);
    assert_eq!(s.intent, Intent::RelativeColorimetric);
    assert!(s.substitute.iter().all(|v| (0.0..=1.0).contains(v)));
    assert_ne!(s.substitute, s.original);
    assert!(s.delta_e76.is_finite() && s.delta_e76 > 1.0, "{}", s.delta_e76);
    let warning = proof.gamut_warning(&s.substitute, 1, GamutWarningStyle::default(), &cancel()).unwrap();
    assert_eq!(warning.out_of_gamut_pixels, 0);
    // Shown on an sRGB display, the substitute is the clipped original.
    let shown = wide(P3, SRGB, &s.substitute, Intent::RelativeColorimetric, false);
    let clipped: Vec<f32> = wide(P3, SRGB, &green, Intent::RelativeColorimetric, false).iter().map(|v| v.clamp(0.0, 1.0)).collect();
    assert!(shown.iter().zip(&clipped).all(|(a, b)| (a - b).abs() < 1e-3), "{shown:?} vs {clipped:?}");
    // A further out-of-gamut colour moves further.
    let red = proof.in_gamut_substitute(&[1.0, 0.0, 0.0], &cancel()).unwrap();
    assert!(red.out_of_gamut && red.substitute_in_gamut && red.delta_e76 > 1.0);
    // The substitute is explicit: a proof run never changes the document, and a hostile colour is refused.
    assert_eq!(proof.in_gamut_substitute(&[0.5, 0.5], &cancel()).unwrap_err(), ProofError::Wide(WideError::ChannelCount));
    assert_eq!(proof.in_gamut_substitute(&[0.5; 6], &cancel()).unwrap_err(), ProofError::Wide(WideError::ChannelCount));
    assert_eq!(proof.in_gamut_substitute(&[0.5, 2.0, 0.5], &cancel()).unwrap_err(), ProofError::Wide(WideError::ChannelRange));
    // Table proof device: blue is out of the ink gamut and gets a finite substitute with a large delta.
    let cmyk = ProofCondition::working_cmyk(input(&pid, CMYK));
    let cmyk = prepare_soft_proof(input(&did, SRGB), &cmyk, input(&sid, SRGB), &cancel()).unwrap();
    let blue = cmyk.in_gamut_substitute(&[0.0, 0.0, 1.0], &cancel()).unwrap();
    assert!(blue.out_of_gamut && blue.delta_e76 > GAMUT_ROUND_TRIP_DELTA_E76, "{blue:?}");
    assert!(blue.substitute.iter().all(|v| (0.0..=1.0).contains(v)));
}
