//! The host hashes the approved plugin file in bounded memory (R6 `unbounded_buffer`): a 48 MiB file is
//! probed while this thread's peak live allocation is metered; the SHA-256 the host computed must equal the
//! one-shot digest of the same bytes (otherwise the probe would be `hash_mismatch`). The metered thread is
//! the caller's: it runs `verify_content`; the worker is another process. This binary holds one test so no
//! other thread competes for the meter.
mod common;

use common::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_score::sha256;
use hsk_studio_score_plugin::*;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

thread_local! {
    static ON: Cell<bool> = const { Cell::new(false) };
    static LIVE: Cell<i64> = const { Cell::new(0) };
    static PEAK: Cell<i64> = const { Cell::new(0) };
}

struct Meter;

fn note(delta: i64) {
    let _ = ON.try_with(|on| {
        if on.get() {
            let live = LIVE.with(|l| {
                l.set(l.get().saturating_add(delta));
                l.get()
            });
            PEAK.with(|p| p.set(p.get().max(live)));
        }
    });
}

// SAFETY: forwards to `System` unchanged; only thread-local integers are updated.
unsafe impl GlobalAlloc for Meter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            note(i64::try_from(layout.size()).unwrap_or(i64::MAX));
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        note(-i64::try_from(layout.size()).unwrap_or(i64::MAX));
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let moved = unsafe { System.realloc(ptr, layout, new_size) };
        if !moved.is_null() {
            note(
                i64::try_from(new_size).unwrap_or(i64::MAX)
                    - i64::try_from(layout.size()).unwrap_or(i64::MAX),
            );
        }
        moved
    }
}

#[global_allocator]
static METER: Meter = Meter;

fn peak_of<T>(body: impl FnOnce() -> T) -> (T, i64) {
    LIVE.with(|l| l.set(0));
    PEAK.with(|p| p.set(0));
    ON.with(|on| on.set(true));
    let result = body();
    ON.with(|on| on.set(false));
    (result, PEAK.with(Cell::get))
}

#[test]
fn host_hashes_a_large_plugin_file_in_bounded_memory() {
    bounded_case("host_hashes_a_large_plugin_file_in_bounded_memory", || {
        let len = 48_usize << 20;
        let bytes: Vec<u8> = (0..len)
            .map(|i| u8::try_from((i.wrapping_mul(193) ^ (i >> 10)) % 251).unwrap_or(0))
            .collect();
        let expected = sha256(&bytes); // one-shot oracle, outside the metered window
        let file = PluginFile::with_bytes(&bytes);
        drop(bytes);
        let host = host();
        let cancel = CancellationToken::default();
        let grant = |sha: [u8; 32]| PluginGrant {
            actor: actor(),
            content_sha256: sha,
        };

        // Matching digest: the probe succeeds, so the streamed digest equals the one-shot digest.
        let (probe, peak) = peak_of(|| {
            host.probe(
                &file.approved_with(expected),
                Some(&grant(expected)),
                &cancel,
                DEADLINE,
            )
        });
        probe.unwrap();
        assert!(
            peak < 4 << 20,
            "host held {peak} bytes at once while hashing a {len} byte file"
        );

        // A different digest is refused with the same bounded memory, and before any worker is spawned.
        let other = sha256(b"other");
        let spawned = host.stats().spawned;
        let (refused, peak) = peak_of(|| {
            host.probe(
                &file.approved_with(other),
                Some(&grant(other)),
                &cancel,
                DEADLINE,
            )
        });
        assert_eq!(refused.unwrap_err(), PluginError::HashMismatch);
        assert!(peak < 4 << 20, "mismatch path held {peak} bytes");
        assert_eq!(host.stats().spawned, spawned, "a mismatch spawns no worker");
    });
}
