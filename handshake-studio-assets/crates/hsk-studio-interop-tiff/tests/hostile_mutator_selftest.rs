//! SMB-PARSER-HOSTILE-1 item 5: self-test of the shared mutation sweep (`support/hostile_mutator.rs`),
//! run once here, in its owning crate, never in the consuming crates (GLOBAL-VALIDATION-006/007).
#[path = "support/hostile_mutator.rs"]
mod hostile_mutator;

use hostile_mutator::*;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

const SEED_INPUT: &[u8] = b"II*\0\x08\0\0\0 <svg width=\"120\" height=\"3.5\"> /Length 4096 obj 8BIM ";
const PROFILE: Profile<'static> = Profile { dictionary: &[b"8BIM", b"<<", b"/Length", b"</g>"], text: true };

/// FNV-1a digest of the first 1,000 mutants of `SEED_INPUT` from seed constant 1 (each mutant's
/// length then bytes), measured on the first run of this file and pinned: a change to any
/// operation, offset rule or random source changes it, so rounds compare like with like.
const DIGEST_1000: u64 = 0xe297_fa76_b569_715f;

#[test]
fn the_first_1000_mutants_are_pinned() {
    let mut rng = Rng::new(1);
    let mut h = FNV_OFFSET;
    let mut changed = 0usize;
    for _ in 0..1000 {
        let m = mutate(&mut rng, SEED_INPUT, &PROFILE);
        changed += usize::from(m != SEED_INPUT);
        h = fnv1a(h, &(m.len() as u64).to_le_bytes());
        h = fnv1a(h, &m);
    }
    println!("MUTATOR digest_1000={h:#018x} changed={changed}");
    assert_eq!(h, DIGEST_1000, "digest of the first 1000 mutants");
    // Almost every mutant differs from its seed (an op can land as a no-op, e.g. a 0-length truncate
    // of an empty tail).
    assert!(changed > 900, "{changed} of 1000 mutants differ");
}

#[test]
fn the_same_constant_gives_the_same_mutants_and_another_differs() {
    let a: Vec<Vec<u8>> = { let mut r = Rng::new(7); (0..50).map(|_| mutate(&mut r, SEED_INPUT, &PROFILE)).collect() };
    let b: Vec<Vec<u8>> = { let mut r = Rng::new(7); (0..50).map(|_| mutate(&mut r, SEED_INPUT, &PROFILE)).collect() };
    let c: Vec<Vec<u8>> = { let mut r = Rng::new(8); (0..50).map(|_| mutate(&mut r, SEED_INPUT, &PROFILE)).collect() };
    assert_eq!(a, b);
    assert_ne!(a, c);
}

#[test]
fn half_the_offsets_fall_in_the_first_512_bytes() {
    // A 64 KiB zero seed: a set byte or flipped bit lands at its offset; count where.
    let seed = vec![0u8; 64 << 10];
    let mut rng = Rng::new(3);
    let (mut head, mut tail) = (0usize, 0usize);
    for _ in 0..2000 {
        let m = mutate(&mut rng, &seed, &Profile { dictionary: &[], text: false });
        if m.len() != seed.len() {
            continue;
        }
        if let Some(i) = m.iter().position(|b| *b != 0) {
            if i < 512 { head += 1 } else { tail += 1 }
        }
    }
    // Uniform over 64 KiB would put under 1 % in the head; the rule puts about half there.
    assert!(head * 3 > head + tail, "head {head} tail {tail}");
}

#[test]
fn the_sweep_counts_panics_codes_peaks_and_reach() {
    let seeds = vec![b"abcdefgh".to_vec(), b"0123456789".to_vec()];
    let report = sweep("selftest", 11, &seeds, 300, PROFILE, 1 << 20, |b| match b.first().copied() {
        Some(b'a') => Ok(()),
        Some(b'0') => Err("zero"),
        Some(0xFF) => panic!("planted"),
        Some(_) if b.len() > 9 => {
            let big = vec![0u8; 2 << 20];
            std::hint::black_box(&big);
            Err("big")
        }
        Some(_) => Err("other"),
        None => Err("empty"),
    });
    println!("{}", report.line());
    assert_eq!(report.mutants, 600);
    assert_eq!(report.ok + report.refused, 600);
    assert!(report.ok > 0 && report.codes.len() >= 3, "{report:?}");
    assert!(report.nondeterministic.is_empty());
    // Panics and the 2 MiB allocation are seen (the cap is 1 MiB).
    assert!(report.panics.iter().all(|(_, _, b)| b.first() == Some(&0xFF)));
    assert_eq!(report.over_cap.is_empty(), !report.codes.contains_key("big"));
    assert!(report.peak >= 2 << 20 || !report.codes.contains_key("big"));
}
