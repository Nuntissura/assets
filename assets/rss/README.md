---
file_id: rss-manual-v1
file_kind: package-manual
updated_at: 2026-10-23
---

<topic id="contracts">

# RSS

Rust source and acquisition contracts for independently hosted RSS. hsscrap owns item identity, capture and immutable observations. RSS owns subscriptions, access partitions, source appearances and independent read/bookmark state. Host adapters authorize operations and supply network, credentials, storage and durable checkpoint commits.

The current foundation is `rss-contracts`. From repository root, build with `cargo build --manifest-path assets/rss/Cargo.toml`. Focused contract proof uses `cargo test --manifest-path assets/rss/Cargo.toml -p rss-contracts --test contracts`. Follow preparation and coordinated build rules first. No service or credentials are needed for these checks. Provider acquisition, GUI, persistence and host adoption are incomplete.

AccessPartition validates public versus connected mode. Public profile, Reels, board and channel modes remain distinct. Connected Following, actual Home/For You, private sources, own uploads and subscription import cannot substitute for one another. Capability availability is an adapter declaration, not proof of source access. VerifiedEmpty requires explicit selected-source evidence; failures, expiry and partial acquisition retain separate states.

Continuation binds subscription, source, access partition, executor and generation. It cannot be used as a WallCursor. CheckpointOwner records accepted item-owner outcomes and source appearances before acknowledging the continuation. A lost response requires scoped operation lookup; pending or unknown outcomes do not authorize advancing a checkpoint. No credential bytes or database/HTTP-framework types appear in these contracts. Optional hsscrap extensions and nullable legacy history remain intact.

</topic>

<topic id="research">

Source inspected 2026-10-23 before implementation: [RSSHub Instagram adapter](https://github.com/DIYgod/RSSHub/blob/master/lib/routes/instagram/web-api/utils.ts), [gallery-dl Instagram](https://github.com/mikf/gallery-dl/blob/master/gallery_dl/extractor/instagram.py), [gallery-dl Pinterest](https://github.com/mikf/gallery-dl/blob/master/gallery_dl/extractor/pinterest.py), [yt-dlp TikTok](https://github.com/yt-dlp/yt-dlp/blob/master/yt_dlp/extractor/tiktok.py), [feed-rs parser](https://github.com/feed-rs/feed-rs/blob/master/feed-rs/src/parser/mod.rs), and [Karakeep feed worker](https://github.com/karakeep-app/karakeep/blob/main/apps/workers/workers/feedWorker.ts).

These implementations separate source identity, parsing, access failures and queued acquisition. RSSHub demonstrates current Instagram profile/GraphQL shapes but its single-page mapping does not prove pagination or anonymous media access. Reuse hsscrap typed IDs and canonical observations, exact Serde versions and explicit owner ports. Reject a second item schema, copied credentials, Python provider-kit substitution, source cursors reused as wall cursors, and empty-login success. Finite host bounds, scoped continuations and explicit uncertain commits address drift, resource exhaustion and skipped arrivals. Contract tests cover those distinctions; later provider and host gates must independently prove actual source access and durable writes.

</topic>
