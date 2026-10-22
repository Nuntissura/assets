---
file_id: cabinet-peak-manual-v1
file_kind: module-manual
updated_at: 2026-10-14
---

<topic id="use">

Peak is an independent, optional hover preview asset. Its default crate has no dependencies and performs no file, network or database access. Hosts supply authorized stable `Key` values, display names and typed tags. Call `hover` on a row or model-focused target; `leave` otherwise. Compact shows name and tags only. `tick` emits one `PreviewRequest` only at expanded stage, after both configured delays. Decode in an owned bounded worker, then return `PreviewCompletion`. Requests are independent of Cabinet's durable mutation slot. Escape calls `dismiss`; the same target stays dismissed until pointer leave. Late completions for replaced resource/context/request identities are rejected.

`Settings::valid` validates settings. Compact and expanded presentation share data; expansion occurs after `hover_delay_ms + expand_delay_ms`. The separate `cabinet-peak-egui` crate provides `cabinet_peak_egui::PeakView::show`, which consumes egui's Window geometry and resizing. The host passes the row anchor each frame and retains it while the returned rectangle is hovered. Resized dimensions update settings. The host persists settings if desired.

The optional `images` feature decodes PNG, JPEG, WebP, GIF (first image), BMP, TIFF, ICO, TGA, PNM, QOI, HDR, EXR, DDS and Farbfeld. UTF-8 and BOM-marked UTF-16 text previews work with any source language as plain text. Binary/control-rich input is unavailable; unknown encoding is explicit. Text and images respect input/output bounds. No syntax execution or external viewer launches occur.

The optional `raw` feature attempts embedded previews, then thumbnails using Rawler. `decode_raw_with_options(..., true)` explicitly opts into sensor development if neither is present; it is disabled by default. Fuji RAF, Nikon NEF/NRW, Canon CR2/CR3/CRW, Sony ARW/SRF/SR2, Samsung SRW and DNG are format candidates; each actual model/compression needs sample proof. No real camera-model specimens have been acquired or tested in this asset session. Apple ProRAW and Samsung Expert RAW are DNG variants, not universal compatibility guarantees. HEIC/HEIF, AVIF and JPEG-XL use host providers. LibRaw is not installed or linked automatically.

Source decoding has a separate `max_decode_pixels` ceiling (default 100 million, maximum 200 million) from `max_output_pixels` (default 4 million). The input ceiling defaults to 128 MiB and hosts may lower it. Image headers and RAW development headers are checked against the decode ceiling; accepted sources are downscaled without enlargement to fit configured popup dimensions and the output pixel ceiling. Common image decoder orientation is applied before downscaling. Camera-specific RAW orientation/color metadata still needs sample proof.

Malformed data, missing embedded previews and disabled providers produce typed `Unsupported`/`Failed` content. Diagnostics are bounded. Native decoding allocates the source image before thumbnailing. `image` allocation limits are best-effort; Rawler may panic or allocate before a pixel bound is known. Panic capture does not prevent abort/OOM. Strict decode deadlines/memory isolation require a bounded host worker process. UI timeout or discarded result never proves decoding stopped. Host must preserve worker ownership and resource limits through completion.

The host supplies its own GUI or embeds the separate adapter; `cabinet-peak` has no GUI feature or framework dependency.

Validation targets: default core timing/fence/text tests; images corrupt header/pixel limit/representative codecs; raw representative camera/model/compression and malformed input; egui hover/expansion/Escape/resize/context change. Native x64/ARM proofs belong to the consuming host. This asset's source is not a Handshake integration verdict.

</topic>

<topic id="bounded-host-scheduling">

`PreviewScheduler` is an optional host-facing workflow using only the standard library. Create one per Peak interaction context with `SchedulerSettings` queue/concurrency/cache ceilings. Each frame call `peak.tick_scheduled(&mut scheduler, now_ms)`. Cache hits complete Peak immediately. Call `take_ready` to obtain owned `PreviewJob` tokens with requests and captured decode settings. Decode through the host's worker adapter, then call `scheduler.finish(token, content)` and `peak.complete(completion)` when a completion is returned. Never decode inside the frame callback.

Queue capacity rejects excess work explicitly. Concurrency counts all active workers, including cancellation pending. On target change, queued stale work is discarded and `take_cancellations` emits active tokens once. A cancellation request does not mean stopped: free ownership with `acknowledge_cancelled(token)` only after the host proves termination, or call `finish` when the worker naturally returns. Cancelled results cannot populate the cache or complete Peak. Host shutdown must retain and join/terminate actual owned workers; dropping this state is no stop proof.

Cache entries use the full resource/context/revision key and captured decode settings, LRU promotion, byte-accounted content/key payload plus metadata allowance, and an entry ceiling. Full context revision fencing conservatively misses the cache after unrelated tree revisions. A changed scope or generation clears old cached payloads and cancels old workspace work; ordinary hover leave preserves the cache. Failed decodes are not cached. Configure zero cache bytes/entries to disable caching; `clear_cache` supports host invalidation. Filesystem watchers, access control, queue deadlines, isolation and actual worker cancellation belong to the host. Peak opens no files and starts no processes.

Research: [Tokio blocking task contract](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html) requires explicit CPU concurrency controls and says started blocking tasks cannot be aborted. [LRU API](https://docs.rs/lru/latest/lru/struct.LruCache.html) provides entry-count eviction; this small bounded scheduler also needs byte accounting. Selected standard-library bounded FIFO/LRU without adding runtime dependencies. Validation: actual hover→dispatch→decode→completion→cache-hit workflow, revision miss, queue rejection, byte/entry eviction and cancelled-slot retention tests in `src/scheduler.rs`; independent host tests should prove worker isolation and cancellation acknowledgments.

</topic>

<topic id="research-basis">

Research checked current primary sources before implementation: [egui0.33.3 interaction](https://docs.rs/egui/0.33.3/egui/style/struct.Interaction.html), [image codecs](https://docs.rs/image/latest/image/codecs/), [image limits](https://docs.rs/image/latest/image/struct.Limits.html), [Rawler0.8 decoder API](https://docs.rs/rawler/0.8.0/rawler/decoders/trait.Decoder.html), [RawSource](https://docs.rs/rawler/0.8.0/rawler/rawsource/struct.RawSource.html), [DNGLab source and limitations](https://github.com/dnglab/dnglab), [LibRaw camera list](https://www.libraw.org/supported-cameras), [LibRaw release limits](https://www.libraw.org/download), [Tokio blocking cancellation](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html).

Selected: effect-free timing/fencing, dependency-free text, optional Rust image/RAW codecs and host providers, egui-owned popup/window mechanics. Rejected: universal language/RAW promises, UI-thread decoding, native LibRaw auto-installation, treating `spawn_blocking.abort` as cancellation, unknown HEIC/AVIF support, whole-camera development by default. Reuse opportunities: host media workers, existing auth/resource identity, diagnostics and theme tokens. Risks and controls: explicit per-model proof, byte/pixel/line ceilings, stale fences, queue/concurrency ownership, worker isolation where strict bounds are required.

</topic>
