---
file_id: rss-instagram-manual-v1
file_kind: package-manual
updated_at: 2026-10-24
---

<topic id="research">

Current source inspected before implementation on 2026-10-23: [gallery-dl Instagram at 882c8d0](https://github.com/mikf/gallery-dl/blob/882c8d0bb5c5a3ed4e88726d2f42a1cb7db59bf2/gallery_dl/extractor/instagram.py#L900-L910) resolves `data.user` using `web_profile_info`. Its [profile and clips methods](https://github.com/mikf/gallery-dl/blob/882c8d0bb5c5a3ed4e88726d2f42a1cb7db59bf2/gallery_dl/extractor/instagram.py#L981-L998) use distinct GET profile and POST clips endpoints. [Pagination](https://github.com/mikf/gallery-dl/blob/882c8d0bb5c5a3ed4e88726d2f42a1cb7db59bf2/gallery_dl/extractor/instagram.py#L1038-L1068) uses profile `more_available/next_max_id` and clips `paging_info.more_available/max_id`. Its media parser preserves carousel order and selects image/video representations. [RSSHub](https://github.com/DIYgod/RSSHub/blob/master/lib/routes/instagram/web-api/utils.ts) uses changing GraphQL document IDs and maps an initial 33 profile edges without exposing subsequent pages. Those IDs and its Stories reel endpoint are unsuitable substitutes for profile Reels pagination.

Reuse RSS source/continuation contracts and hsscrap observations and identity owners. This crate implements Rust parsing and request construction. It does not execute Python or guess GraphQL IDs. Public transport may be unavailable, challenged or rate limited. There is no automatic connected-session fallback. Host policies enforce anonymous/public transport, authority, retained response snapshots, network address restrictions, deadlines and retries. Strict bounded JSON parsing, numeric account verification on every call, mode separation and digest-bound resume address schema drift, account retargeting and lost page responses. Fixture proof covers these contracts. Real access to the requested account, full history reconciliation and host persistence remain independent gates.

</topic>

<topic id="use">

`InstagramProvider<F, C>` implements `rss_contracts::ProviderPort` for `PublicProfile` and `PublicReels`. Select the exact profile URL or its `/reels/` view. Both resolve one numeric account; their traversal coverage remains distinct. Inject `PublicFetchPort` and `CanonicalEntryPort`. The host must prepare through existing `rss-acquisition::prepare_entry`, resolve reliable provider identity `instagram/media/<numeric-id>` and allocate canonical item, observation and ordered gallery member/reference IDs. Provider IDs and media IDs are evidence only.

`public::resolve_source(url, requested_mode, bounds)` exposes the provider's existing source validation and returns `(normalized_url, lowercase_username)`. The normalized URL uses HTTPS, `www.instagram.com` and a trailing slash; `PublicProfile` accepts only the profile path and `PublicReels` only its `/reels/` path. Other modes, hosts, credentials, queries and fragments reject. This pure lookup performs no network access, resolves no numeric account and grants no source authority; acquisition still verifies the current account and request.

Each transport call carries the complete source request and host execution reference. The host authorizes immediately before network and owner effects and rejects expired attempts. Retain exact request/response bytes before canonical preparation. An expected digest selects that retained document; missing or changed snapshots fail and cannot silently substitute a fresh page. The host may return HTTP 304 only with the exact authorized cached body. Pagination tokens bind numeric account, source URL, mode, source revision, executor and generation through the outer continuation, provider cursor, digest/offset and bounded prior identities. Each new page operation has its own authorized execution attempt. Persist the complete pending page and exact canonical preparation results so retries do not create new operation intents. Canonical stored continuations are opaque host state; the host never accepts an arbitrary caller-edited token as authoritative traversal history.

An exhausted nonempty traversal reports Partial because the public API cannot establish an immutable full-history snapshot. A verified terminal empty page after continuation settles only that traversal. An initially empty profile is VerifiedEmpty only when the verified public account explicitly reports zero media and the profile endpoint explicitly ends. Empty Reels or ambiguous empty pages fail without clearing earlier data. Publication times preserve source Unix seconds and UTC precision; absent dates remain absent. Media URLs are expiring locators. The host must retain expiry/re-fetch behavior and merge Partial media evidence without deleting unmentioned canonical members.

After registration, the coordinated build owner runs `cargo test --locked --manifest-path rss/Cargo.toml -p rss-provider-instagram --tests` from the repository root under the active preparation gate. Tests use disposable callbacks and synthetic provider JSON. They do not establish real source availability, durable writes, browser rendering or host adoption. No live provider call is made by this library's tests.

</topic>
