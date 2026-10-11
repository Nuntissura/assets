//! Release-mode, JSONL measurements; no files or host resources are modified.
use cabinet::*;
use std::{hint::black_box, time::Instant};

// Benchmark-only instrumentation delegates every allocation to System unchanged.
// Counts are observed allocator calls/requested bytes, not RSS or a safety guarantee.
mod heap {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
    pub static CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static LIVE: AtomicUsize = AtomicUsize::new(0);
    pub static PEAK: AtomicUsize = AtomicUsize::new(0);
    pub struct Counting;
    fn added(bytes: usize) {
        let live = LIVE.fetch_add(bytes, Relaxed).wrapping_add(bytes);
        PEAK.fetch_max(live, Relaxed);
    }
    // SAFETY: original pointers/layouts pass directly to System; accounting uses
    // only nonallocating atomics and never reads or modifies allocation contents.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let ptr = unsafe { System.alloc(layout) };
            if !ptr.is_null() {
                CALLS.fetch_add(1, Relaxed);
                added(layout.size());
            }
            ptr
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            let ptr = unsafe { System.alloc_zeroed(layout) };
            if !ptr.is_null() {
                CALLS.fetch_add(1, Relaxed);
                added(layout.size());
            }
            ptr
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) };
            LIVE.fetch_sub(layout.size(), Relaxed);
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            let next = unsafe { System.realloc(ptr, layout, new_size) };
            if !next.is_null() {
                CALLS.fetch_add(1, Relaxed);
                if new_size >= layout.size() {
                    added(new_size - layout.size());
                } else {
                    LIVE.fetch_sub(layout.size() - new_size, Relaxed);
                }
            }
            next
        }
    }
    pub fn begin() -> (usize, usize) {
        let calls = CALLS.load(Relaxed);
        let live = LIVE.load(Relaxed);
        PEAK.store(live, Relaxed);
        (calls, live)
    }
    pub fn end(before: (usize, usize)) -> (usize, usize, usize) {
        (
            CALLS.load(Relaxed).saturating_sub(before.0),
            LIVE.load(Relaxed),
            PEAK.load(Relaxed).saturating_sub(before.1),
        )
    }
}
#[global_allocator]
static ALLOCATOR: heap::Counting = heap::Counting;

fn snapshot(size: usize, shape: &str) -> Snapshot {
    let depth = size.min(128);
    let entries = (0..size)
        .map(|i| {
            let (parent, kind) = match shape {
                "wide" => {
                    if i % 1001 == 0 {
                        (None, EntryKind::Folder)
                    } else {
                        (Some(format!("e{}", i / 1001 * 1001)), EntryKind::Leaf)
                    }
                }
                "deep" => {
                    if i < depth {
                        (i.checked_sub(1).map(|p| format!("e{p}")), EntryKind::Folder)
                    } else {
                        (Some(format!("e{}", depth - 1)), EntryKind::Leaf)
                    }
                }
                _ => (None, EntryKind::Leaf),
            };
            let mut e = Entry::new(format!("e{i}"), parent, format!("File-{i:09}"), kind);
            e.order = i as i64;
            e
        })
        .collect();
    Snapshot {
        context: Context {
            scope: "benchmark".into(),
            generation: 1,
            revision: 1,
        },
        hierarchy_complete: true,
        entries,
    }
}
fn measure<T>(size: usize, shape: &str, state: &str, operation: &str, f: impl FnOnce() -> T) -> T {
    let memory = heap::begin();
    let start = Instant::now();
    let result = f();
    let elapsed = start.elapsed().as_nanos();
    let (allocation_calls, live_heap_bytes, peak_extra_heap_bytes) = heap::end(memory);
    let max_depth = max_depth(size, shape);
    println!("{{\"schema\":\"cabinet-benchmark-v1\",\"nodes\":{size},\"shape\":\"{shape}\",\"max_depth\":{max_depth},\"state\":\"{state}\",\"operation\":\"{operation}\",\"elapsed_ns\":{elapsed},\"allocation_calls\":{allocation_calls},\"live_heap_bytes\":{live_heap_bytes},\"peak_extra_heap_bytes\":{peak_extra_heap_bytes},\"instrumented\":true}}");
    result
}
// Root depth is zero; deep has a bounded 128-folder spine plus leaves.
fn max_depth(size: usize, shape: &str) -> usize {
    match shape {
        "wide" => usize::from(size > 1),
        "deep" => size.min(128).min(size.saturating_sub(1)),
        _ => 0,
    }
}
fn diagnostics(size: usize, shape: &str, phase: &str, c: &Cabinet) {
    let d = c.diagnostics();
    println!("{{\"schema\":\"cabinet-benchmark-v1\",\"nodes\":{size},\"shape\":\"{shape}\",\"operation\":\"diagnostics\",\"phase\":\"{phase}\",\"loaded\":{},\"projected\":{},\"rendered\":{},\"projection_rebuilds\":{}}}", d.loaded,d.projected,d.rendered,d.projection_rebuilds);
}
#[cfg(feature = "egui")]
fn frames(size: usize, shape: &str, c: &mut Cabinet) {
    use cabinet::egui_view::CabinetView;
    c.select("e0", SelectionModifiers::default()).unwrap();
    let ctx = egui::Context::default();
    let mut view = CabinetView::new("benchmark-view");
    let mut elapsed = Vec::new();
    let mut mounted_max = 0;
    let mut stationary_ids = Vec::new();
    let mut scroll_pointer = egui::pos2(300.0, 300.0);
    let mut scroll_applicable = false;
    let mut changed_viewport_frames = 0;
    let memory = heap::begin();
    for frame in 0..20 {
        let start = Instant::now();
        let _ = ctx.run(
            egui::RawInput {
                time: Some(frame as f64 / 60.0),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::Vec2::new(700.0, 500.0),
                )),
                events: if frame >= 10 && scroll_applicable {
                    vec![
                        egui::Event::PointerMoved(scroll_pointer),
                        egui::Event::MouseWheel {
                            unit: egui::MouseWheelUnit::Point,
                            delta: egui::vec2(0.0, -120.0),
                            modifiers: Default::default(),
                        },
                    ]
                } else {
                    vec![]
                },
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let response = view.show(ui, c);
                    mounted_max = mounted_max.max(response.rows.len());
                    if frame >= 9 {
                        let mut ids: Vec<_> = response.rows.keys().cloned().collect();
                        ids.sort_unstable();
                        if frame == 9 {
                            scroll_applicable = c.projected_rows().len() > ids.len();
                            let mut centers: Vec<_> =
                                response.rows.values().map(|rect| rect.center()).collect();
                            centers.sort_by(|a, b| a.y.total_cmp(&b.y));
                            if !centers.is_empty() {
                                scroll_pointer = centers[centers.len() / 2];
                            }
                            stationary_ids = ids;
                        } else if ids != stationary_ids {
                            changed_viewport_frames += 1;
                        }
                    }
                });
            },
        );
        elapsed.push(start.elapsed().as_nanos());
    }
    elapsed.sort_unstable();
    let (allocation_calls, live_heap_bytes, peak_extra_heap_bytes) = heap::end(memory);
    let scroll_input_frames = if scroll_applicable { 10 } else { 0 };
    println!("{{\"schema\":\"cabinet-benchmark-v1\",\"nodes\":{size},\"shape\":\"{shape}\",\"operation\":\"egui-frames\",\"frames\":20,\"stationary_frames\":10,\"scroll_input_frames\":{scroll_input_frames},\"scroll_applicable\":{scroll_applicable},\"changed_viewport_frames\":{changed_viewport_frames},\"p50_ns\":{},\"p95_ns\":{},\"max_ns\":{},\"mounted_max\":{mounted_max},\"allocation_calls\":{allocation_calls},\"live_heap_bytes\":{live_heap_bytes},\"peak_extra_heap_bytes\":{peak_extra_heap_bytes},\"instrumented\":true}}",elapsed[9],elapsed[18],elapsed[19]);
    assert!(
        !scroll_applicable || changed_viewport_frames > 0,
        "scrollable benchmark must observe changed mounted identities after wheel input"
    );
    diagnostics(size, shape, "after-egui", c);
}
fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let sizes: Vec<usize> = if args.is_empty() {
        vec![10_000, 100_000, 1_000_000]
    } else {
        args.iter()
            .map(|arg| arg.parse().expect("arguments must be positive node counts"))
            .collect()
    };
    for size in sizes {
        assert!(size > 0);
        for shape in ["flat", "wide", "deep"] {
            let s = measure(size, shape, "canonical", "fixture", || {
                snapshot(size, shape)
            });
            let mut c = measure(size, shape, "collapsed", "new", || Cabinet::new(s).unwrap());
            measure(size, shape, "collapsed", "rows-cold", || {
                black_box(c.rows())
            });
            measure(size, shape, "collapsed", "rows-warm-3", || {
                for _ in 0..3 {
                    black_box(c.rows());
                }
            });
            measure(
                size,
                shape,
                "collapsed",
                "shared-projection-warm-1000",
                || {
                    for _ in 0..1000 {
                        black_box(c.projected_rows());
                    }
                },
            );
            diagnostics(size, shape, "warm-projection", &c);
            let last = format!("e{}", size - 1);
            measure(size, shape, "collapsed", "lookup-last-1000", || {
                for _ in 0..1000 {
                    black_box(c.entry(&last));
                }
            });
            if shape != "flat" {
                measure(size, shape, "collapsed", "reveal-last", || {
                    c.reveal(&last).unwrap()
                });
            }
            measure(size, shape, "partly-expanded", "select-last", || {
                c.select(&last, SelectionModifiers::default()).unwrap()
            });
            measure(size, shape, "partly-expanded", "selected-warm-3", || {
                for _ in 0..3 {
                    black_box(c.selected());
                }
            });
            let request = measure(size, shape, "partly-expanded", "request-open-last", || {
                c.request(Operation::Open { id: last.clone() }).unwrap()
            });
            println!("{{\"schema\":\"cabinet-benchmark-v1\",\"nodes\":{size},\"shape\":\"{shape}\",\"operation\":\"request-fence-count\",\"expected_revisions\":{}}}", request.expected_revisions.len());
            // The benchmark never dispatches this intention to a host.
            c.abandon_pending(request.id).unwrap();
            measure(size, shape, "filtered", "filter-last", || {
                c.set_filter(&format!("File-{:09}", size - 1))
            });
            let filtered = measure(size, shape, "filtered", "rows-filtered", || c.rows());
            println!("{{\"schema\":\"cabinet-benchmark-v1\",\"nodes\":{size},\"shape\":\"{shape}\",\"operation\":\"filtered-row-count\",\"rows\":{}}}", filtered.len());
            measure(size, shape, "expanded", "clear-filter", || c.set_filter(""));
            if shape == "wide" {
                measure(size, shape, "expanded", "expand-all-folders", || {
                    for i in (0..size).step_by(1001) {
                        c.expand(&format!("e{i}"), true).unwrap();
                    }
                });
            }
            measure(size, shape, "expanded", "rows-expanded", || {
                black_box(c.rows())
            });
            #[cfg(feature = "egui")]
            frames(size, shape, &mut c);
            let mut changed = c.entry(&last).unwrap().clone();
            changed.revision += 1;
            changed.label = "Delta-last".into();
            let batch = DeltaBatch {
                base: c.snapshot().context.clone(),
                revision: c.snapshot().context.revision + 1,
                expected_revisions: [(last.clone(), changed.revision - 1)].into_iter().collect(),
                upserts: vec![changed],
                removed: vec![],
                hierarchy_complete: None,
            };
            measure(size, shape, "canonical", "delta-one-label", || {
                c.apply_delta(batch).unwrap()
            });
            measure(
                size,
                shape,
                "delta-refreshed",
                "projection-after-delta",
                || black_box(c.projected_rows()),
            );
            diagnostics(size, shape, "after-delta", &c);
            let mut update = measure(size, shape, "canonical", "clone-for-update", || {
                c.snapshot().clone()
            });
            update.context.revision += 1;
            update.entries[size - 1].revision += 1;
            update.entries[size - 1].label = "Changed-last".into();
            measure(size, shape, "canonical", "replace-one-label", || {
                c.replace(update).unwrap()
            });
            measure(size, shape, "refreshed", "rows-after-refresh", || {
                black_box(c.rows())
            });
            diagnostics(size, shape, "after-full-refresh", &c);
        }
    }
}
