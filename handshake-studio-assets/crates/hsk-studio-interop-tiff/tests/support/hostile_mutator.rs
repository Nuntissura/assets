//! Deterministic hostile-input mutation sweep (SMB-PARSER-HOSTILE-1 item 5), std only, included by
//! `#[path]` from the parser crates' tests (one copy, no crate, no `Cargo.toml` line; precedent
//! `harness-view/tests/viewer_attribution.rs`). Its self-test runs once, in this crate
//! (`tests/hostile_mutator_selftest.rs`).
//!
//! - Random source: xorshift64* from a fixed constant per target; no clock, no environment.
//! - Offsets: half within the first 512 bytes (where length fields live), half anywhere.
//! - Operations, uniform: truncate; set one byte from {0, 1, 0x7F, 0x80, 0xFF}; flip one bit;
//!   overwrite 2, 4 or 8 bytes in either byte order with 0, 1, 0x7F.., 0x80.., 0xFF.. or the seed's
//!   length; erase a block; insert a block of one repeated byte; copy a block inside the input;
//!   replace or insert a token of the target's dictionary; for text formats, replace a decimal
//!   number with one of {0, -1, 1e308, 4294967295}. 1 to 4 per mutant.
//! - Worker: the whole loop runs on an explicit 2 MiB thread (F-R56-1 used 2 MiB; Rust's default
//!   may change). Each input runs under `catch_unwind`: a panic fails the sweep and is printed with
//!   target, seed, index and the bytes as hex when under 4 KiB.
//! - Abort or hang: nextest's process per test and 120 s cap are the gate. `HOSTILE_TRACE=1` prints
//!   each index before it runs (reproduction only; it changes nothing else).
//! - Allocation: [`CountingAlloc`], a global-atomic live/peak counter (the parser may spawn its own
//!   threads), installed by each target binary with `#[global_allocator]`; the peak live bytes of any
//!   one input above the target's cap fails the sweep.
//! - Reach: [`Report::line`] prints `MUTATION target=.. seeds=.. mutants=.. ok=.. refused=..
//!   caught_panics=.. codes={..} peak=.. slowest_ms=..`; [`Report::assert_reach`] requires ok of at
//!   least 1 and of at least the floor the target measured, and at least 3 distinct refusal codes,
//!   so a sweep that dies at the magic number fails (V4-03).
//! - Determinism: fixed counts; the first 200 mutants are generated and run again and must give the
//!   same outcome codes.
#![allow(dead_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicIsize, Ordering};

/// xorshift64* (Vigna). A zero state is replaced by a fixed non-zero constant.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed })
    }
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// Uniform-ish in `0..n` (`0` when `n` is 0).
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next() % n as u64) as usize }
    }
}

/// How a target's bytes may be damaged.
#[derive(Clone, Copy, Debug)]
pub struct Profile<'a> {
    /// Tokens the target's format gives meaning (tags, keys, operators, chunk names).
    pub dictionary: &'a [&'a [u8]],
    /// The format is text: decimal numbers are also swapped.
    pub text: bool,
}

const BYTES: [u8; 5] = [0, 1, 0x7F, 0x80, 0xFF];
const NUMBERS: [&[u8]; 4] = [b"0", b"-1", b"1e308", b"4294967295"];

fn offset(rng: &mut Rng, len: usize) -> usize {
    if rng.below(2) == 0 { rng.below(len.min(512)) } else { rng.below(len) }
}

fn block(rng: &mut Rng, len: usize) -> usize {
    1 + rng.below(len.clamp(1, 256))
}

/// Applies one operation to `out` (`seed_len`: the seed's length, one of the overwrite values).
fn apply(rng: &mut Rng, out: &mut Vec<u8>, seed_len: usize, profile: &Profile<'_>) {
    let ops = if profile.text { 10 } else { 9 };
    let len = out.len();
    match rng.below(ops) {
        0 => out.truncate(offset(rng, len)),
        1 if len > 0 => {
            let at = offset(rng, len);
            out[at] = BYTES[rng.below(BYTES.len())];
        }
        2 if len > 0 => {
            let at = offset(rng, len);
            out[at] ^= 1 << rng.below(8);
        }
        3 if len > 0 => {
            let width = [2usize, 4, 8][rng.below(3)];
            let value: u64 = match rng.below(6) {
                0 => 0,
                1 => 1,
                2 => 0x7F << ((width - 1) * 8) | (u64::MAX >> (64 - (width - 1) * 8)),
                3 => 0x80 << ((width - 1) * 8),
                4 => u64::MAX >> (64 - width * 8),
                _ => seed_len as u64,
            };
            let bytes: Vec<u8> = if rng.below(2) == 0 { value.to_le_bytes()[..width].to_vec() } else { value.to_be_bytes()[8 - width..].to_vec() };
            let at = offset(rng, len);
            for (i, b) in bytes.iter().enumerate() {
                if let Some(slot) = out.get_mut(at + i) {
                    *slot = *b;
                }
            }
        }
        4 if len > 0 => {
            let at = offset(rng, len);
            let n = block(rng, len - at);
            out.drain(at..(at + n).min(len));
        }
        5 => {
            let at = offset(rng, len + 1).min(len);
            let n = block(rng, 256);
            let b = BYTES[rng.below(BYTES.len())];
            out.splice(at..at, std::iter::repeat_n(b, n));
        }
        6 if len > 1 => {
            let from = rng.below(len);
            let n = block(rng, len - from);
            let piece: Vec<u8> = out[from..(from + n).min(len)].to_vec();
            let at = offset(rng, len);
            if rng.below(2) == 0 {
                for (i, b) in piece.iter().enumerate() {
                    if let Some(slot) = out.get_mut(at + i) {
                        *slot = *b;
                    }
                }
            } else {
                out.splice(at..at, piece);
            }
        }
        7 | 8 if !profile.dictionary.is_empty() => {
            let token = profile.dictionary[rng.below(profile.dictionary.len())];
            let at = offset(rng, len + 1).min(len);
            if rng.below(2) == 0 {
                let end = (at + token.len()).min(len);
                out.splice(at..end, token.iter().copied());
            } else {
                out.splice(at..at, token.iter().copied());
            }
        }
        9 => {
            // Replace the decimal number at or after a random offset.
            let at = offset(rng, len);
            if let Some(start) = (at..len).find(|i| out[*i].is_ascii_digit()) {
                let mut end = start;
                while end < len && (out[end].is_ascii_digit() || out[end] == b'.') {
                    end += 1;
                }
                let start = if start > 0 && out[start - 1] == b'-' { start - 1 } else { start };
                out.splice(start..end, NUMBERS[rng.below(NUMBERS.len())].iter().copied());
            }
        }
        _ => {}
    }
}

/// One mutant of `seed`: 1 to 4 operations.
pub fn mutate(rng: &mut Rng, seed: &[u8], profile: &Profile<'_>) -> Vec<u8> {
    let mut out = seed.to_vec();
    for _ in 0..1 + rng.below(4) {
        apply(rng, &mut out, seed.len(), profile);
    }
    out
}

/// 64-bit FNV-1a over `bytes`, continuing from `h`.
pub fn fnv1a(mut h: u64, bytes: &[u8]) -> u64 {
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}
pub const FNV_OFFSET: u64 = 0xCBF2_9CE4_8422_2325;

// ----- allocation -----------------------------------------------------------------------------------

static LIVE: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicIsize = AtomicIsize::new(0);

/// Live and peak heap bytes over every thread of the process. Install in a target binary with
/// `#[global_allocator] static A: CountingAlloc = CountingAlloc;`.
pub struct CountingAlloc;

fn grow(delta: isize) {
    let now = LIVE.fetch_add(delta, Ordering::Relaxed) + delta;
    PEAK.fetch_max(now, Ordering::Relaxed);
}

// SAFETY: every method forwards to the system allocator unchanged; the counters only observe.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            grow(layout.size() as isize);
        }
        p
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            grow(layout.size() as isize);
        }
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            grow(new_size as isize - layout.size() as isize);
        }
        p
    }
}

// ----- sweep ----------------------------------------------------------------------------------------

/// The answer of one input: `Ok` or a refusal code.
pub type Answer = Result<(), &'static str>;

/// What a sweep saw.
#[derive(Debug, Default)]
pub struct Report {
    pub target: &'static str,
    pub seeds: usize,
    pub mutants: usize,
    pub ok: usize,
    pub refused: usize,
    /// Refusal codes that name a panic the parser caught itself (a typed answer, third-party
    /// cause; counted, not a failure).
    pub caught_panics: usize,
    pub codes: BTreeMap<&'static str, usize>,
    /// Largest peak of live heap bytes above the start of any one input.
    pub peak: usize,
    pub slowest_ms: u128,
    /// Panics that reached the sweep: `(seed, index, bytes)`.
    pub panics: Vec<(usize, usize, Vec<u8>)>,
    /// Inputs whose peak passed the cap: `(seed, index, peak)`.
    pub over_cap: Vec<(usize, usize, usize)>,
    /// Indexes whose second run answered differently.
    pub nondeterministic: Vec<usize>,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl Report {
    pub fn line(&self) -> String {
        let codes: Vec<String> = self.codes.iter().map(|(c, n)| format!("{c}:{n}")).collect();
        format!(
            "MUTATION target={} seeds={} mutants={} ok={} refused={} caught_panics={} codes={{{}}} peak={} slowest_ms={}",
            self.target,
            self.seeds,
            self.mutants,
            self.ok,
            self.refused,
            self.caught_panics,
            codes.join(","),
            self.peak,
            self.slowest_ms
        )
    }

    /// Panics, cap breaches and nondeterminism fail; reach must be at least `min_ok` ok answers
    /// (and 1) and 3 distinct refusal codes.
    pub fn assert_reach(&self, min_ok: usize) {
        println!("{}", self.line());
        for (seed, index, bytes) in &self.panics {
            let shown = if bytes.len() < 4096 { hex(bytes) } else { format!("<{} bytes>", bytes.len()) };
            println!("PANIC target={} seed={seed} index={index} bytes={shown}", self.target);
        }
        assert!(self.panics.is_empty(), "{}: {} inputs panicked", self.target, self.panics.len());
        assert!(self.over_cap.is_empty(), "{}: peak over cap {:?}", self.target, self.over_cap);
        assert!(self.nondeterministic.is_empty(), "{}: answers differ on rerun {:?}", self.target, self.nondeterministic);
        assert!(self.ok >= min_ok.max(1), "{}: ok {} under the reach floor {min_ok}", self.target, self.ok);
        assert!(self.codes.len() >= 3, "{}: {} distinct refusal codes, need 3", self.target, self.codes.len());
    }
}

/// A sweep over `seeds`: `per_seed` mutants of each, from `seed_const`, each answered by `run`,
/// with peak live heap bytes per input checked against `cap_bytes` (module docs).
pub fn sweep<F>(target: &'static str, seed_const: u64, seeds: &[Vec<u8>], per_seed: usize, profile: Profile<'_>, cap_bytes: usize, run: F) -> Report
where
    F: Fn(&[u8]) -> Answer + Sync,
{
    let trace = std::env::var("HOSTILE_TRACE").is_ok_and(|v| v == "1");
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name(format!("hostile-{target}"))
            .stack_size(2 << 20)
            .spawn_scoped(scope, || {
                let hook = std::panic::take_hook();
                std::panic::set_hook(Box::new(|_| {}));
                let mut report = Report { target, seeds: seeds.len(), ..Report::default() };
                let mut first: Vec<Answer> = Vec::new();
                let mut rng = Rng::new(seed_const);
                let mut index = 0usize;
                for (s, seed) in seeds.iter().enumerate() {
                    for _ in 0..per_seed {
                        let input = mutate(&mut rng, seed, &profile);
                        if trace {
                            eprintln!("HOSTILE_TRACE target={target} seed={s} index={index}");
                        }
                        let base = LIVE.load(Ordering::Relaxed);
                        PEAK.store(base, Ordering::Relaxed);
                        let started = std::time::Instant::now();
                        let answer = catch_unwind(AssertUnwindSafe(|| run(&input)));
                        report.slowest_ms = report.slowest_ms.max(started.elapsed().as_millis());
                        let peak = usize::try_from(PEAK.load(Ordering::Relaxed) - base).unwrap_or(0);
                        report.peak = report.peak.max(peak);
                        if peak > cap_bytes {
                            report.over_cap.push((s, index, peak));
                        }
                        report.mutants += 1;
                        let answer = match answer {
                            Ok(a) => a,
                            Err(_) => {
                                report.panics.push((s, index, input));
                                Err("panic")
                            }
                        };
                        match answer {
                            Ok(()) => report.ok += 1,
                            Err(code) => {
                                report.refused += 1;
                                *report.codes.entry(code).or_insert(0) += 1;
                                if code.contains("panicked") {
                                    report.caught_panics += 1;
                                }
                            }
                        }
                        if first.len() < 200 {
                            first.push(answer);
                        }
                        index += 1;
                    }
                }
                // Determinism: the first 200 mutants again.
                let mut rng = Rng::new(seed_const);
                let mut index = 0usize;
                'again: for seed in seeds {
                    for _ in 0..per_seed {
                        if index >= first.len() {
                            break 'again;
                        }
                        let input = mutate(&mut rng, seed, &profile);
                        let answer = catch_unwind(AssertUnwindSafe(|| run(&input))).unwrap_or(Err("panic"));
                        if answer != first[index] {
                            report.nondeterministic.push(index);
                        }
                        index += 1;
                    }
                }
                std::panic::set_hook(hook);
                report
            })
            .expect("sweep thread")
            .join()
            .expect("sweep thread")
    })
}
