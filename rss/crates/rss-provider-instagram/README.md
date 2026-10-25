---
file_id: rss-instagram-manual-v1
file_kind: package-manual
updated_at: 2026-10-25
---

<topic id="research">

Current source inspected before implementation on 2026-10-23: [gallery-dl Instagram at 882c8d0](https://github.com/mikf/gallery-dl/blob/882c8d0bb5c5a3ed4e88726d2f42a1cb7db59bf2/gallery_dl/extractor/instagram.py#L900-L910) resolves `data.user` using `web_profile_info`. Its [profile and clips methods](https://github.com/mikf/gallery-dl/blob/882c8d0bb5c5a3ed4e88726d2f42a1cb7db59bf2/gallery_dl/extractor/instagram.py#L981-L998) use distinct GET profile and POST clips endpoints. [Pagination](https://github.com/mikf/gallery-dl/blob/882c8d0bb5c5a3ed4e88726d2f42a1cb7db59bf2/gallery_dl/extractor/instagram.py#L1038-L1068) uses profile `more_available/next_max_id` and clips `paging_info.more_available/max_id`. Its media parser preserves carousel order and selects image/video representations. [RSSHub](https://github.com/DIYgod/RSSHub/blob/master/lib/routes/instagram/web-api/utils.ts) uses changing GraphQL document IDs and maps an initial 33 profile edges without exposing subsequent pages. Those IDs and its Stories reel endpoint are unsuitable substitutes for profile Reels pagination.

Reuse RSS source/continuation contracts and hsscrap observations and identity owners. This crate implements Rust parsing and request construction. It does not execute Python or guess GraphQL IDs. Public transport may be unavailable, challenged or rate limited. There is no automatic connected-session fallback. Host policies enforce anonymous/public transport, authority, retained response snapshots, network address restrictions, deadlines and retries. Strict bounded JSON parsing, numeric account verification on every call, mode separation and digest-bound resume address schema drift, account retargeting and lost page responses. Fixture proof covers these contracts. Real access to the requested account, full history reconciliation and host persistence remain independent gates.

</topic>

<topic id="public-request-metadata" status="in_progress">

On2026-10-25, Portal's actual public profile/Reels probe received HTTP400 before retaining any document. This proves access failure, not its cause. The current [gallery-dl REST request implementation](https://github.com/mikf/gallery-dl/blob/master/gallery_dl/extractor/instagram.py#L1018-L1037) supplies public application/AJAX/referer metadata that the original FetchRequest contract omitted. Its session/CSRF/claim/cookie fields and browser impersonation are not adopted.

`FetchRequest::public_metadata()` returns one fixed typed `PublicRequestMetadata`: honest crate/version User-Agent, Accept, public application ID, fixed public root Referer and AJAX marker. `header_pairs()` exposes exactly those five fields; `validate()` rejects changed values, and unknown fields fail deserialization. Hosts retain the complete metadata with method/form in request intent, keys and snapshots, validate it before current fetch and recovery, and send the same values through injected and production transports. No arbitrary headers or connected credentials are accepted. Legacy omitted metadata remains distinct and cannot be silently reused as the new request.

Owning metadata/serialization and host binding tests precede a changed actual public-source probe. These headers are an evidence-backed protocol hypothesis; accessor tests do not prove source access or full traversal.

</topic>

<topic id="authorized-public-sources" status="in_progress">

Research checked on 2026-10-25: [gallery-dl's session and redirect handling](https://github.com/mikf/gallery-dl/blob/882c8d0bb5c5a3ed4e88726d2f42a1cb7db59bf2/gallery_dl/extractor/instagram.py#L139-L179) keeps authentication in its transport and distinguishes login/challenge redirects. [Instaloader's request context](https://github.com/instaloader/instaloader/blob/master/instaloader/instaloadercontext.py#L579-L625) likewise owns session state outside media traversal. [Meta's official API collection](https://www.postman.com/meta/instagram/documentation/6yqw8pt/instagram-api) describes professional-account API limitations; replacing ordinary-customer source access with professional-account conversion is unsuitable. Direct Meta documentation retrieval returned HTTP429, so that unavailable page supplies no additional evidence.

Portal's exact anonymous profile/Reels requests now receive HTTP401 at identity lookup. The selected extension is an explicit authorized transport for those same public content modes. Hosts retain secrets and the opaque credential lease, validate account/profile/provider/login identity, expiry and revocation, and hold their lease guard through canonical settlement. The asset carries only a bounded nonsecret authorization context and rechecks it around fetch and canonical preparation. The existing normalizer, traversal, media identity and pagination are shared with the anonymous adapter. Silent cookie fallback, credential ownership in this crate, copied parsers and a new authentication framework are rejected.

The stable connection reference belongs to the subscription partition. Lease generation belongs to one attempt and may change on ordinary renewal. An authorized continuation can therefore use a fresh lease generation while retaining its original exact body digest and request provenance; it must not relabel old response metadata or fetch replacement bytes for a missing retained body. Public adapters continue denying any connection-bound request. Other public modes retain their existing partition rules.

`AuthorizedInstagramProvider<F, C>` consumes `AuthorizedFetchPort::{current, fetch}` and the existing `CanonicalEntryPort`. `AuthorizedContext` contains only `connection_ref`, `owner_scope`, the logged-in account's numeric `external_identity`, and `lease_generation`. The logged-in identity is distinct from the selected target profile. `current` surrounds each fetch and each canonical preparation. A constructed authorized provider advertises the two implemented public content modes; the host selects it only for an explicitly bound connection and still proves actual lease validity on every callback.

Owning proof covers mode/connection admission, distinct login and selected target identities, public-only transport rejection, stale/revoked context before canonical effects, and a continuation across lease renewal. Host proof must independently establish real login, exact retained request recovery and canonical acquisition. Personal Following/Home, private sources and own uploads remain required later work under MT011; this selected-source extension does not complete them.

The coordinated build owner runs `cargo test --locked --manifest-path rss/Cargo.toml -p rss-contracts -p rss-provider-instagram -p rss-provider-tiktok -p rss-provider-pinterest --tests` for this boundary extension. The TikTok and Pinterest guards preserve their anonymous-only admission when the shared public-profile partition domain gains an optional explicit connection; Pinterest discovery and all-pins remain separate tested paths.

</topic>

<topic id="use">

`InstagramProvider<F, C>` implements `rss_contracts::ProviderPort` for `PublicProfile` and `PublicReels`. Select the exact profile URL or its `/reels/` view. Both resolve one numeric account; their traversal coverage remains distinct. Inject `PublicFetchPort` and `CanonicalEntryPort`. The host must prepare through existing `rss-acquisition::prepare_entry`, resolve reliable provider identity `instagram/media/<numeric-id>` and allocate canonical item, observation and ordered gallery member/reference IDs. Provider IDs and media IDs are evidence only.

`public::resolve_source(url, requested_mode, bounds)` exposes the provider's existing source validation and returns `(normalized_url, lowercase_username)`. The normalized URL uses HTTPS, `www.instagram.com` and a trailing slash; `PublicProfile` accepts only the profile path and `PublicReels` only its `/reels/` path. Other modes, hosts, credentials, queries and fragments reject. This pure lookup performs no network access, resolves no numeric account and grants no source authority; acquisition still verifies the current account and request.

Each transport call carries the complete source request and host execution reference. The host authorizes immediately before network and owner effects and rejects expired attempts. Retain exact request/response bytes before canonical preparation. An expected digest selects that retained document; missing or changed snapshots fail and cannot silently substitute a fresh page. The host may return HTTP 304 only with the exact authorized cached body. Pagination tokens bind numeric account, source URL, mode, source revision, executor and generation through the outer continuation, provider cursor, digest/offset and bounded prior identities. Each new page operation has its own authorized execution attempt. Persist the complete pending page and exact canonical preparation results so retries do not create new operation intents. Canonical stored continuations are opaque host state; the host never accepts an arbitrary caller-edited token as authoritative traversal history.

An exhausted nonempty traversal reports Partial because the public API cannot establish an immutable full-history snapshot. A verified terminal empty page after continuation settles only that traversal. An initially empty profile is VerifiedEmpty only when the verified public account explicitly reports zero media and the profile endpoint explicitly ends. Empty Reels or ambiguous empty pages fail without clearing earlier data. Publication times preserve source Unix seconds and UTC precision; absent dates remain absent. Media URLs are expiring locators. The host must retain expiry/re-fetch behavior and merge Partial media evidence without deleting unmentioned canonical members.

After registration, the coordinated build owner runs `cargo test --locked --manifest-path rss/Cargo.toml -p rss-provider-instagram --tests` from the repository root under the active preparation gate. Tests use disposable callbacks and synthetic provider JSON. They do not establish real source availability, durable writes, browser rendering or host adoption. No live provider call is made by this library's tests.

</topic>
