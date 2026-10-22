---
file_id: handshake-embed-views-readme
file_kind: package-readme
updated_at: 2026-10-18
---

<topic id="package-manual">

# handshake_embed_views

Existing image rendering, texture cache, slideshow/album state and video activation helpers extracted from Handshake MT-014. The manifest and all five Rust files are unchanged, retaining 17 inline tests. Dependencies remain `egui = "0.33"`, `image = "0.25"` with default features disabled and only png/jpeg/webp/gif enabled, and sibling `handshake_asset_resolver` at `../handshake_asset_resolver`. This package includes an egui adapter. It does not implement a host editor or media service.

## API and use

```rust
use handshake_embed_views::slideshow_view::SlideshowViewState;
use handshake_embed_views::album_view::{AlbumViewState, ALBUM_COLUMNS, row_count};

let mut slides = SlideshowViewState::new();
slides.prev(3);
assert_eq!(slides.current_index, 2);
slides.next(3);
assert_eq!(slides.current_index, 0);
let mut album = AlbumViewState::new();
album.open(2, 3);
assert_eq!(album.open_index, Some(2));
assert_eq!(row_count(7, ALBUM_COLUMNS), 3);
```

`image_view::decode_rgba` accepts encoded image bytes and returns `Result<egui::ColorImage, handshake_asset_resolver::EmbedError>`. It checks encoded length, image dimensions and pixel count against resolver-owned limits before decoding. Unsupported/corrupt images and zero dimensions return `MediaLoadFailed`; resource violations return `ResourceLimit`. The function is synchronous. The host must schedule expensive decode away from the UI thread; this crate does not spawn that work.

`EmbedTextureCache` owns egui texture handles keyed by asset ID. `upload` uses the egui context and returns an existing handle when the key is already cached. The host calls it on the egui thread after decode. `contains`, `get`, `len`, `is_empty`, `remove` and `clear` inspect or invalidate the cache. Clear it when changing workspaces, because asset IDs are workspace-local. Replace or remove a key before uploading changed pixels for that asset. `aspect_fit_size` keeps the aspect ratio without upscaling beyond intrinsic width. `render_image` consumes a texture and resolver `ResolvedAsset`, attaches hover text from the original filename or asset ID, and returns an egui response. `ImageInteraction::{Clickable, Static}` selects click or hover sense; the host owns accessibility actions and overlays.

`SlideshowViewState` stores the current member index. `next` and `prev` clamp then wrap within the caller-supplied sequence length; empty sequences reset to index zero. `clamped_index` returns an index safe for the current length, while the host must handle an empty sequence before indexing. Helpers generate container/prev/next author IDs and extract the first comma-separated reference token. The host supplies ordered members and renders navigation.

`AlbumViewState` stores an optional open member index. `open` clamps the index or closes for an empty sequence; `close` and `is_open` manage the state. `ALBUM_COLUMNS` is three. `row_count` and `grid_position` calculate layout with at least one column. Helpers generate grid/cell author IDs. The host renders the thumbnail grid and full-size modal; this module supplies state and arithmetic.

`video_view` supplies `VideoViewState`, `VideoPlayHandler`, `InlineRevealPlayHandler`, `VideoActivation` and author-ID helpers. The default handler returns `ShowUrlInline`. `OsOpenReserved` is a reserved value, not an implemented external launch. There is no in-process video decoder or player here. The host renders the poster/play controls, invokes the handler on a click and reveals the URL inline.

## Recovery and proof boundaries

The host owns fetching/resolution, workspace routing, decode task cancellation, UI lifecycle, state persistence, accessibility wiring and user actions. This package neither fetches media nor starts processes. On decode failure, retain the typed resolver error and render the host error state. Re-resolve or refetch before retry when source bytes change; clear/remove stale texture handles. Reinitialize per-embed navigation and modal state when the host changes document identity.

This assignment produced source only. No compilation, tests, lock generation, GPU/UI launch, decode execution or runtime proof was run. The 17 retained test declarations are unexecuted. Standalone dependency/source proof, required GUI/runtime acceptance, normal publication and independent canonical remote reread remain outstanding. Separate host adoption must consume a verified immutable source revision and prove its own integration. Existing host source remains the recovery point until then.

</topic>
