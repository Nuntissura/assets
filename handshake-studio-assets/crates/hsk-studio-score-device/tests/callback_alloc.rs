//! Dedicated test binary: a counting global allocator proves the realtime callback bodies never
//! allocate. Counting is per thread (a const thread-local flag), so the test harness and other
//! threads cannot pollute the count.
use hsk_studio_score::ChannelLayout;
use hsk_studio_score_device::{
    Counters, Limits, StreamConfig, capture_callback, render_callback, ring,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

struct CountingAllocator;

fn note() {
    if COUNTING.try_with(Cell::get).unwrap_or(false) {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
    }
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note();
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(e) => panic!("case {name} exceeded/lost declared 60s deadline: {e}"),
    }
}

#[test]
fn callback_is_allocation_free_and_lock_free() {
    bounded_case("callback_is_allocation_free_and_lock_free", || {
        let config = StreamConfig {
            sample_rate_hz: 48_000,
            layout: ChannelLayout::stereo(),
            block_frames: 64,
            ring_blocks: 4,
        };
        let g = config.validate(&Limits::default()).unwrap();
        let out_counters = Arc::new(Counters::default());
        let in_counters = Arc::new(Counters::default());
        let (mut producer, mut consumer) = ring(&g, Arc::clone(&out_counters));
        let (mut capture_p, mut capture_c) = ring(&g, Arc::clone(&in_counters));
        let block = vec![0.25_f32; g.block_samples];
        let input = vec![0.5_f32; g.block_samples];
        // 96-frame callback buffer: never block-aligned, so the partial/underrun path runs too.
        let mut out = vec![0.0_f32; 96 * g.channels];
        let mut drained = vec![0.0_f32; g.block_samples];

        // Instrument self-check: a deliberate allocation inside the counting window is seen.
        ALLOCATIONS.store(0, Ordering::Relaxed);
        COUNTING.with(|c| c.set(true));
        let probe = Vec::<u8>::with_capacity(16);
        std::hint::black_box(&probe);
        COUNTING.with(|c| c.set(false));
        assert!(ALLOCATIONS.load(Ordering::Relaxed) >= 1, "counting allocator is blind");
        drop(probe);

        ALLOCATIONS.store(0, Ordering::Relaxed);
        COUNTING.with(|c| c.set(true));
        for i in 0..10_000_u32 {
            if i % 3 != 0 {
                let _ = producer.try_push_block(&block);
            }
            render_callback(&mut consumer, &mut out);
            capture_callback(&mut capture_p, &input);
            if i % 5 == 0 {
                let _ = capture_c.pop_into(&mut drained);
            }
        }
        COUNTING.with(|c| c.set(false));
        let allocations = ALLOCATIONS.load(Ordering::Relaxed);

        assert_eq!(allocations, 0, "callback bodies allocated");
        // Both the copy path and the underrun / overrun paths really ran.
        assert_eq!(out_counters.callbacks(), 10_000);
        assert!(out_counters.consumed_frames() > 0 && out_counters.underruns() > 0);
        assert_eq!(in_counters.callbacks(), 10_000);
        assert!(in_counters.submitted_frames() > 0 && in_counters.overruns() > 0);

        // Source scan: the callback path (callback bodies, ring ends, counters) contains no
        // locking, growth, formatting, printing or sleeping.
        let sources = [
            ("callback.rs", include_str!("../src/callback.rs")),
            ("ring.rs", include_str!("../src/ring.rs")),
            ("counters.rs", include_str!("../src/counters.rs")),
        ];
        let forbidden = [
            "Mutex", "RwLock", "Vec::push", ".push(", "format!", "println!", "eprintln!",
            "vec!", ".to_vec(", ".to_string(", "thread::sleep", ".lock(",
        ];
        for (file, text) in sources {
            for needle in forbidden {
                assert!(!text.contains(needle), "{file} contains `{needle}`");
            }
        }
    });
}
