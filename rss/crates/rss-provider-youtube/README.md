---
file_id: rss-youtube-manual-v1
file_kind: package-manual
updated_at: 2026-10-24
---

<topic id="research">

Research inspected on 2026-10-23 before implementation: Google's [channels.list](https://developers.google.com/youtube/data/reference/rest/v3/channels/list), [playlistItems.list](https://developers.google.com/youtube/data/reference/rest/v3/playlistItems/list), [playlist item resource](https://developers.google.com/youtube/data/reference/rest/v3/playlistItems), [videos.list](https://developers.google.com/youtube/data/reference/rest/v3/videos/list) and [API setup](https://developers.google.com/youtube/data/getting-started). Channels resolve IDs/handles and identify their uploads playlist. Playlist requests accept up to 50 results and return opaque page tokens. Playlist insertion time is distinct from `contentDetails.videoPublishedAt`. Video IDs identify videos globally. Public API requests need host-managed project credentials but do not require a user's connected YouTube account.

Adjacent source inspected: [yt-dlp tab extraction](https://github.com/yt-dlp/yt-dlp/blob/master/yt_dlp/extractor/youtube/_tab.py#L527-L583) handles renderer families, continuation loops and changing visitor data. The existing Portal channel reader resolves public channel/handle URLs to a recent Atom document. Select the documented uploads API for paginated public history and expose no-login Atom as an explicit recent-snapshot strategy. Reject treating recent XML as complete channel history, hardcoding Innertube visitor/client state, calling a Python downloader, silently switching to connected accounts, and using playlist item IDs as canonical video identity.

Reuse RSS and hsscrap owner contracts, maintained feed-rs Atom parsing and the published strict JSON admission helper. Host-owned bounded transport and retained snapshots isolate keys, authorization, redirects, DNS rules and caches. Digest-bound offsets, scoped continuations, cycle rejection and stable global video identities address retries, retargeting and repeated pages. Finite limits fail without advancing canonical checkpoints. Native callbacks prove adapter behavior; real selected-channel access, quota availability, complete canonical reconciliation and host persistence require separate gates.

</topic>

<topic id="use">

`YouTubeProvider<F,C>` implements public-channel acquisition. Choose `Strategy::PublicApi` for uploads pagination and individual public video metadata, or `Strategy::NoLoginAtom` for the current channel feed. The choice is explicit. Atom feed IDs may contain the full requested UC channel ID or its exact tail; the tail form additionally requires an alternate link resolving to the full requested canonical channel. Video permalinks reuse the native URL resolver and must resolve to the exact entry video ID, including Shorts. Canonical evidence and identity remain youtube/video with watch URLs. Public API transport supplies backend project credentials outside this crate and never includes them in request DTOs, cursors or source URLs. Atom transport attaches no connected-user credentials. Connected Home, Following, private sources and subscription imports are separate unfinished modes.

Inject `FetchPort` and `CanonicalEntryPort`. Authorize the source, account/profile scope, executor and attempt before transport and canonical effects. Reuse `rss-acquisition::prepare_entry` and the canonical identity owner with the global identity `youtube/video/<video-id>`. The host allocates canonical IDs and persists ordered media references. Embed URLs are provider player references, thumbnails are previews, and neither proves downloaded video bytes. Merge their Partial gallery evidence without deleting existing members.

`public::resolve_channel_html(bytes, bounds, max_body_bytes)` reuses the acquisition parser to resolve one unique canonical channel from retained handle HTML. The body limit must be nonzero and at most 16 MB. It rejects missing or conflicting canonical channel links, invalid UTF-8 and oversized bodies. It performs no network request. The host must retain and verify the exact authorized handle request, response URL and bytes before using this resolver to bind a channel during acquisition or recovery; HTML alone is not source authority.

Retain raw responses under exact request/operation intent before canonical preparation. An expected digest requires the retained document and must fail if missing or changed. HTTP 304 requires exact authorized cached bytes. Keep continuations in canonical host storage; do not trust caller-edited traversal state. Cursor scope, source revision, channel/playlist identity, provider token, digest/offset and bounded seen IDs are checked before owner effects. One acknowledged page advances through the existing atomic checkpoint boundary. Mutable public traversal and recent XML remain Partial even when no more pages are advertised. Empty feeds alone do not establish an empty channel; errors retain prior data. Only an explicitly verified zero-count uploads response can establish empty API coverage.

After coordinated manifest registration, run `cargo test --locked --manifest-path rss/Cargo.toml -p rss-provider-youtube --tests` under the active preparation gate. Fixture tests use synthetic responses and disposable canonical callbacks. No credentials, live network, database or browser are used by those tests. On missing keys, quota/rate limits, expiry, restricted/deleted entries or changed snapshots, retain the existing checkpoint and report the typed failure or Partial result. Host adoption and actual source acquisition remain unproven until their own gates pass.

</topic>
