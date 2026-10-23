---
file_id: rss-tiktok-manual-v1
file_kind: package-manual
updated_at: 2026-10-23
---

<topic id="research">

Current primary sources inspected before implementation on 2026-10-23: [yt-dlp TikTok at 9b58cb3](https://github.com/yt-dlp/yt-dlp/blob/9b58cb33d0cd3e823c8d8732f6adce8faa1f37e3/yt_dlp/extractor/tiktok.py#L998-L1130), [gallery-dl TikTok](https://github.com/mikf/gallery-dl/blob/882c8d0bb5c5a3ed4e88726d2f42a1cb7db59bf2/gallery_dl/extractor/tiktok.py#L67-L92) and [official creator embed documentation](https://developers.tiktok.com/docs/en/embed-creator-profiles). The official embed selects at most ten recent videos. It cannot establish full pagination. yt-dlp reads verified user identity from the public rehydration document, then traverses `creator/item_list` with `secUid`, timestamp windows and `hasMorePrevious`. Gallery-dl supports that route and a separate ordered `post/item_list` route, which it describes as cookie-dependent and flaky. Its image-post parser preserves `imagePost.images` order and image URLs/dimensions.

Select the explicit public creator-window protocol and individual public item rehydration. Reject embed-only history, silently changing endpoint or connected mode, fabricated device IDs, copying a Python worker or executing a downloader. Rust owns URL resolution, raw JSON/HTML normalization, pagination and canonical evidence. Genuine public browser/device context, network signatures, rate policy, address/redirect policy and credentials remain host effects. Reuse RSS/hsscrap contracts, the published strict JSON admission helper and maintained HTML parsing.

Provider endpoints and anonymous access can change or be challenged. A malformed/empty successful HTTP response is not an empty creator. Validate account identity, every page entry and continuation before owner preparation; retain digest-bound page snapshots and finite traversal history. Dates come from explicit `createTime`, never from a bit-shifted item ID. Canonical identity is global `tiktok/item/<post-id>`; member positions are ordering evidence, never invented stable media IDs. Meaningful public-API fixtures cover these invariants; actual public access and canonical host reconciliation remain independent gates.

</topic>

<topic id="use">

`TikTokProvider<F,C>` acquires a selected public creator or individual `/@creator/video/<id>` or `/photo/<id>` URL. Inject bounded `PublicFetchPort`, actual host time through `initial_cursor_millis`, and `CanonicalEntryPort`. The host supplies genuine public browser/request context when the provider requires it; it must not attach a connected user's session or replace Rust normalization with another implementation. Keys and browser state are never embedded in source URLs, provider cursors or fixtures.

The host authorizes scope, source, generation, executor and attempt before each fetch and canonical effect. It retains exact raw responses under immutable page-operation intent. An expected digest selects the retained snapshot and fails if missing or different. Canonical callbacks use existing `rss-acquisition::prepare_entry`, resolve the global provider identity and allocate all item, observation and ordered media IDs. Apply Partial gallery evidence through the owning gallery boundary while preserving existing members. Media URLs are expiring public locators; byte retrieval and expiry recovery belong to the host.

Public creator traversal follows the inspected timestamp-window protocol. Empty continuing windows are consumed within the current bounded acquisition call, because the current checkpoint owner cannot acknowledge empty continuing Partial pages. Do not fabricate such an acknowledgement. Set modest finite fetch/document budgets, ordinarily a small number of requests, and enforce the actual attempt deadline. Exhaustion fails without checkpoint advancement. A larger configured ceiling is only a ceiling, not proof that sparse history makes progress. Actual sparse-history runtime acceptance must verify that the host's budget reaches an item or terminal window. The adapter never issues a giant default burst and has no automatic retry or route fallback.

Nonempty traversal remains Partial even at advertised exhaustion, because public timestamp windows cannot prove an immutable complete archive. A terminal empty page reached from a canonical continuation settles only that traversal. An initial empty result requires verified public profile statistics explicitly reporting zero posts and an actual terminal response; otherwise it fails. Connected Following, For You and private sources remain separate unfinished modes.

After coordinated registration, the build owner runs `cargo test --locked --manifest-path assets/rss/Cargo.toml -p rss-provider-tiktok --tests` under the active preparation gate. Synthetic raw responses and disposable callbacks prove only the adapter contract. No live source, browser session, persistent database, canonical owner integration or host adoption is claimed by those fixtures.

</topic>
