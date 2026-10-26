---
file_id: rss-gui-manual-v1
file_kind: package-manual
updated_at: 2026-10-23
---

<topic id="integration">

# Rust feed wall

`rss-core::query` supplies bounded query, refresh, gallery, lazy-media and player transitions. `rss-gui` embeds an optional egui wall and source manager and composes the published `hsscrap-gui::Inspector`. The host supplies authorization, canonical source selection, database queries, owner projections, textures, players, storage and application layout. The core has no GUI dependency. This component does not adopt itself into Portal or prove live provider access.

Read the canonical library and profile selection, authorize their source references and access partitions, then construct `WallQuery::from_library`. Focused collections must be canonically selected. Direct source selection and visibility inherited through a collection remain separate. Manager actions are requests; update the displayed model only after the owning service confirms its canonical mutation. Source health preserves never-checked, running, verified-empty, partial, reconnect, expired and failed states.

Execute each `PageTicket.request` against the complete authorized query. Apply source/group predicates, text/tag/read/bookmark filters and context before ordering and limiting to 25, 50 or 100 canonical items. One item can have several appearances, but occupies one row. `SortKey` orders normalized numeric UTC milliseconds, unknown dates last, and uses canonical ItemId as a unique tie-breaker. SourcePriority places descending priority first. ProjectSaved requires a selected project context. The host must resolve publication precision and offset without inventing a date.

The opaque `WallSnapshot` must preserve query membership **and order keys** across requests. Use a host mechanism that actually provides this property: a retained snapshot, immutable query materialization or equivalent. A watermark over mutable publication/priority keys is insufficient. Do not assume cross-request database MVCC or a version-enabled engine. Bind opaque cursors to this snapshot and the complete query/authority epoch; reject expiry and authority changes. Never substitute acquisition continuations, OFFSET pagination, a fetched-all client list or a rendered subset. The portable reducer cannot establish that the host returned every canonical match. Host database acceptance is separate from the in-memory fixtures.

After every query replacement, call `WallState::reconcile`, execute its effects, then request bounded owner projections with `request_cards` and complete the exact returned ticket. Rendering emits preview requests only for visible cards. Playback requires activation and replaces the one active player. Execute `StopPlayback` and `InvalidateMedia` by disposing host decoders/textures; do not keep an old player just because its MemberId is unchanged. The host reauthorizes every media operation and never executes arbitrary provider HTML/scripts. Stale card/media/inspection tickets are rejected. AccessRevoked requires revoking the query and clearing host caches too.

Card admission checks the selected observation's access partition against the ticket's authorized query before caching any row in the batch. The host separately authorizes gallery-owner reads: canonical `GalleryMember` records carry an `observation_id`, but no access-partition field. An ID match alone does not authorize gallery metadata, remote locators or retained media.

Call `set_position` with the visible canonical item and a fractional offset. Refresh seeks that item in a new host snapshot; the current snapshot stays visible while paging. Explicit Apply updates replaces it only after a valid response. If the user moved meanwhile, application rejects and the host can request a new refresh at the current anchor. Removed anchors require the host's canonical negative lookup and produce an explicit AnchorRemoved result. Restore the returned position in the host scroll view. Expired snapshots must offer a fresh anchored query, never silently continue against new data.

Card faces omit routine source, timestamp and tag metadata. The information control requests an independently authorized inspection. Its selected membership alone supplies the displayed project save date and inherited tags; an all-context view has no selected membership or invented global project date. The canonical inspector shows project save dates inside Link and capture provenance. Changing context requires a canonical query/context transition. `complete_inspection` validates that boundary, then the existing hsscrap GUI handles draft/save/cancel/conflict/unknown states and exact assignment removal ownership. The overlay inherits the host theme and panel color, keeping text coverage consistent with the renderer's font atlas. The host executes its owner plan and rereads canonical data. The wall does not certify durability from a GUI save result. Escape cancels a local draft before closing; pending or uncertain drafts cannot be dismissed through the wall Close control.

</topic>

<topic id="media-release">

Before evicting a ready, unavailable or failed member, call `WallState::release_media(&MemberRef)` and execute its `ReleaseMedia` and optional `StopPlayback` effects. Dispose that exact member's cached texture/player and abort its pending resolver. The current card must contain the active member; unrelated members, selection and gallery order are preserved. Releasing the same current member is idempotent. A later visible preview request receives a fresh ticket; completing or cancelling the retired ticket rejects as stale. A release is not an access grant: authorize every new owner media read independently.

Research checked 2026-10-26: [MDN object URL release](https://developer.mozilla.org/en-US/docs/Web/API/URL/revokeObjectURL_static) and [resource cancellation](https://developer.mozilla.org/en-US/docs/Web/JavaScript/Guide/Resource_management). Reuse the existing member/ticket lifecycle so host resource disposal and reducer state agree. Reject direct resolver calls outside returned tickets and full-wall resets for one eviction. Native lifecycle cases, actual egui reload and compiled WASM replay cover terminal-state retries, stale replies and exact-member isolation; host authorization and byte budgets remain independent obligations.

</topic>

<topic id="verification">

From `assets/rss`, after the coordinated gameplan and exclusive Cargo handoff:

```powershell
cargo test --locked -p rss-core --test wall_query
cargo test --locked -p rss-gui --test wall -- --test-threads=1 --nocapture
```

The pure target is `fixtures/interaction/query.rs`; it exercises the public API against an explicitly synthetic frozen-set fixture, including canonical versus visible page membership, changed sort keys, refresh anchors, failures, scope fences and actual emitted player/cache effects. It does not prove a host database, network source or durable commit.

The GUI target is `fixtures/interaction/gui.rs`. It drives actual egui controls with keyboard and raw touch, renders through offscreen WGPU and saves PNGs to the checkout's ignored `.local/rss-gui/proof` directory. Override with `RSS_GUI_PROOF_DIR` after moving the checkout. No foreground window, credentials, snapshot autoacceptance or live network service is needed. Every capture identifies its synthetic data. Inspect the actual wide/narrow wall, details, loading, empty, partial, reconnect and failed images before a visual verdict. Local proof passed fourteen pure cases, the original eight GUI cases and focused selected-context and modal-contrast cases. The final GUI target has ten cases. Thirteen actual captures were inspected, and labeled informational glyph cores meet the existing 4.5 contrast criterion in light and dark host themes. Canonical isolated publication, actual Portal adoption and canonical host database pagination remain separate required work.

</topic>

<topic id="research">

Inspected primary patterns on 2026-10-23: [PostgreSQL unique ordered limits](https://www.postgresql.org/docs/18/queries-limit.html), [transaction isolation](https://www.postgresql.org/docs/current/transaction-iso.html), [Microsoft opaque paging](https://learn.microsoft.com/en-us/graph/paging), [Microsoft collection guidelines](https://github.com/microsoft/api-guidelines/blob/vNext/graph/articles/collections.md), [GitHub bounded cursor pagination](https://docs.github.com/en/graphql/guides/using-pagination-in-the-graphql-api), [WAI feed keyboard/navigation pattern](https://www.w3.org/WAI/ARIA/apg/patterns/feed/), [Feedly views](https://docs.feedly.com/article/276-how-do-i-change-the-views-of-my-feeds-and-source), [SurrealDB SELECT/versioning](https://surrealdb.com/docs/reference/query-language/statements/select) and [LIMIT/START scan behavior](https://surrealdb.com/docs/reference/query-language/clauses/limit). The exact egui 0.33.3 Modal implementation was inspected for its foreground input boundary; the hsscrap GUI retains its own edit/Escape behavior.

Reuse existing SourceLibrary selection/rules, canonical hsscrap records and the separately published inspector. Preserve the distinction between acquisition cursors and wall cursors, and between canonical results and loaded rows. Reject client-side slicing and implicit snapshot claims: both can skip or duplicate arrivals under mutable sorting. Keep stable host snapshots, strict order/ID validation, bounded encoded payloads, request generations and explicit media invalidations. Core fixtures expose transition failures quickly; rendered native widget proof checks usability; host database/provider/adoption gates must independently establish their own behavior. No bulk mutation action is introduced by this task. Future actions targeting all matches must carry the canonical selection query, never loaded rows.

</topic>
