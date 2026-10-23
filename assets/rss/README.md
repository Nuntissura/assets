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

<topic id="source-library">

`rss-core` provides pure reducers for account-owned sources, many-to-many groups, source display-name editing, pause/resume, group-specific keyword and priority rules, profile selection and independent appearance state. Run its focused cases with `cargo test --manifest-path assets/rss/Cargo.toml -p rss-core --test catalogue` after the coordinated preparation gate.

Pass an authorized owner scope and expected library revision to each mutation. The returned snapshot is a proposed next state. A host must persist it with an atomic compare-and-swap, reject stale commits and authorize the selected profile. Pure reducer tests do not prove that persistence boundary. Display-name editing preserves provider/source URL identity; changing the followed target uses a new follow intent.

One source selected through several groups schedules once. Removing a group or membership preserves the source and all separately owned captures, item tags and project placements. Per-member rules keep each group's keywords and priority separate; the host supplies finite rule limits and account keyword budget. Existing Portal collection rules supplied the trim, printable-text and case-insensitive uniqueness behavior. Detach removes only that membership's rule. Source and group removal clean their own rules.

Profile selection intersects host-authorized sources before counts or acquisition. Item grouping preserves each appearance-state ID and its exact read/bookmark booleans, including contradictory historical rows. It never aggregates them with OR/AND. Host authorization must also filter access partitions; matching IDs carry no grants.

</topic>

<topic id="research">

Source inspected 2026-10-23 before implementation: [RSSHub Instagram adapter](https://github.com/DIYgod/RSSHub/blob/master/lib/routes/instagram/web-api/utils.ts), [gallery-dl Instagram](https://github.com/mikf/gallery-dl/blob/master/gallery_dl/extractor/instagram.py), [gallery-dl Pinterest](https://github.com/mikf/gallery-dl/blob/master/gallery_dl/extractor/pinterest.py), [yt-dlp TikTok](https://github.com/yt-dlp/yt-dlp/blob/master/yt_dlp/extractor/tiktok.py), [feed-rs parser](https://github.com/feed-rs/feed-rs/blob/master/feed-rs/src/parser/mod.rs), and [Karakeep feed worker](https://github.com/karakeep-app/karakeep/blob/main/apps/workers/workers/feedWorker.ts).

These implementations separate source identity, parsing, access failures and queued acquisition. RSSHub demonstrates current Instagram profile/GraphQL shapes but its single-page mapping does not prove pagination or anonymous media access. Reuse hsscrap typed IDs and canonical observations, exact Serde versions and explicit owner ports. Reject a second item schema, copied credentials, Python provider-kit substitution, source cursors reused as wall cursors, and empty-login success. Finite host bounds, scoped continuations and explicit uncertain commits address drift, resource exhaustion and skipped arrivals. Contract tests cover those distinctions; later provider and host gates must independently prove actual source access and durable writes.

</topic>

<topic id="acquisition">

`rss-acquisition` supplies serializable per-subscription job transitions and the Save URL capture bridge. The host persists each transition with its own atomic revision guard, authorizes the selected account/source, supplies monotonic ticks and enforces the returned deadline. It must persist `begin_commit` before item-owner effects. Running and uncertain jobs both reserve concurrency until canonical reconciliation; cancellation requests do not prove termination. Queue capacity, attempt count, timeout and exponential backoff are finite host inputs. One subscription can have one outstanding page operation across its profile views. Terminal removal does not change the canonical source checkpoint.

`save_url` accepts the existing `CaptureIntent::SaveUrl` and hsscrap `URLCapture`. It checks exact URL intent, actor, scope and command bounds. It reconciles operation identity through `ItemOwner`, drives existing Pending operations through `MutationCoordinator::recover`, and rereads the exact stored capture before returning its committed receipt. Enrichment and following a source are separate operations. Owner ports must enforce idempotency and intent reuse atomically. Pending, lost and unknown outcomes never authorize a new logical operation or source continuation.

Identity-aware Save URL deduplication, source-page ingestion and checkpoint acknowledgement remain unfinished until hsscrap's owner URL/provider identity extensions are independently proven and integrated. The candidate contains no provider acquisition, network client, credential store, database, scheduler service or GUI. Focused candidate proof is `cargo test --locked --manifest-path assets/rss/Cargo.toml -p rss-acquisition --tests` from repository root, under the coordinated build owner and preparation gate. Tests exercise local owner contracts and do not prove host persistence or real provider access.

Current primary-source inspection on 2026-10-23 confirmed that [Karakeep's feed worker](https://github.com/karakeep-app/karakeep/blob/main/apps/workers/workers/feedWorker.ts) uses per-feed idempotency, user grouping and bounded worker concurrency/timeouts. Reuse the existing RSS contracts and hsscrap owner recovery rather than introduce a second queue authority or metadata schema. Keep network scheduling in the host and return deterministic transitions. The main risks are duplicate profile scheduling, expired worker effects and lost owner replies. Subscription uniqueness, executor/deadline checks, retained uncertain reservations and canonical owner rereads address those risks. Queue/recovery tests cover these decisions; durable host and source acquisition proof remains separate.

</topic>
