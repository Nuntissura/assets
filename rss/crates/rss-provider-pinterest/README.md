---
file_id: rss-pinterest-manual-v1
file_kind: package-manual
updated_at: 2026-10-23
---

<topic id="research">

Primary implementation inspected before building on 2026-10-23: [gallery-dl Pinterest at 882c8d0](https://github.com/mikf/gallery-dl/blob/882c8d0bb5c5a3ed4e88726d2f42a1cb7db59bf2/gallery_dl/extractor/pinterest.py#L399-L517). Board, section, pin and all-pins requests use distinct resources and opaque bookmark arrays. Responses contain an array or a `results` array. Explicit terminal markers include `-end-` and the inspected `Y2JOb25lO` prefix. [Media handling](https://github.com/mikf/gallery-dl/blob/882c8d0bb5c5a3ed4e88726d2f42a1cb7db59bf2/gallery_dl/extractor/pinterest.py#L82-L187) distinguishes ordered story pages/blocks, ordered carousel slots, video representations and original images. A user's root profile discovers boards; `/user/pins/` is a separate all-pins feed. Official v5 board/pin documentation was checked but returned only a login shell to this research tool. It does not establish anonymous v5 access.

Reuse RSS/hsscrap contracts, the published strict JSON admission helper and bounded host transport. Select inspected public web resources, preserving source discovery, following a source and capturing a single pin as separate intents. Reject a Python worker dependency, an implicit connected-account fallback, a fixed ten-page truncation reported as complete, inferred original image URLs, and merging different pin IDs merely because their image URLs match. Global identity is `pinterest/pin/<pin-id>`; selected board/section membership is separate from pin authorship.

The official [Pinterest OpenAPI source](https://github.com/pinterest/api-description/blob/main/v5/openapi.yaml) was also inspected directly. It identifies authenticated v5 board scopes and video durations in milliseconds. Those declarations do not prove anonymous web-resource access or the unit of its similarly named fields. This adapter leaves web video duration unknown unless the raw response explicitly supplies milliseconds.

Web endpoints may change, require public browser context or reject anonymous requests. Validate source identity and every returned entry before canonical preparation. Opaque scoped bookmark history, retained response digests and finite host limits address loops, retries, retargeting and resource exhaustion. Mutable traversal remains Partial. Public API fixtures prove only those adapter contracts; actual source access, host persistence and canonical reconciliation need their separate gates.

</topic>

<topic id="use">

`PinterestProvider<F,C>` supports public board URLs, board-section URLs, explicit `/user/pins/` sources and single `/pin/<id>/` item URLs. A root `/user/` URL is board discovery, not silently an all-pins follow. `discover_boards` returns provider board evidence and its own scoped continuation; it never creates canonical items or subscriptions. The host allocates source references when the user follows a discovered board.

Inject `PublicFetchPort` with actual source authorization, public browser/network policy, deadlines and byte bounds. Anonymous anti-CSRF/request context belongs to the host and must not borrow connected-user credentials. Retain exact raw responses per operation before preparation. Expected digests select retained snapshots; missing or changed snapshots fail. HTTP 304 needs exact authorized cached bytes. The host must keep canonical continuations opaque and reauthorize account/profile, source, executor, generation and operation attempts before network and owner effects.

`CanonicalEntryPort` resolves the global pin identity, prepares through `rss-acquisition::prepare_entry` and allocates canonical item, observation and ordered media IDs. Media provider keys are optional evidence; positions never become invented stable IDs. Pin links stay Pinterest pin URLs; an outbound article link does not replace canonical pin identity. Repins with distinct provider IDs remain distinct even when they share an image. Preserve Partial gallery members not mentioned by a later response. Story text/sticker blocks are not fabricated media files; unsupported blocks fail visibly rather than erase the gallery.

After coordinated registration, run `cargo test --locked --manifest-path assets/rss/Cargo.toml -p rss-provider-pinterest --tests` under the active preparation gate. Disposable callbacks and synthetic raw responses need no network or credentials. They do not prove public availability, full immutable history, database commits or host adoption. On challenge, denial, expiry, rate limiting, missing bookmarks or a repeated cursor, retain the previous canonical checkpoint and report the typed failure.

</topic>
