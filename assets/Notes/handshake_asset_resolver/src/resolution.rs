use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use crate::types::*;

/// Validate ONE asset-ref fail-closed (the native mirror of the React `validateAssetRef`,
/// byte-for-byte in CHECK ORDER). Returns `Ok(trimmed_id)` when valid, else the typed
/// [`EmbedError`]. The order matters: a `:` is classified as a drive letter vs a scheme
/// BEFORE the separator/traversal check, exactly as the React code does, so `C:\x` is an
/// `AbsolutePathRejected` and `http://x` is a `SchemeRejected` (not a generic traversal).
pub fn validate_asset_ref(ref_value: &str) -> Result<String, EmbedError> {
    let value = ref_value.trim();
    if value.is_empty() {
        return Err(EmbedError::EmptyRef);
    }
    // A `:` carries either a drive letter (`C:\`, `C:/`) or a scheme (`http://`, `file:`,
    // `javascript:`); a real asset id never contains `:`.
    if value.contains(':') {
        if looks_like_drive_letter(value) {
            return Err(EmbedError::AbsolutePathRejected(value.to_owned()));
        }
        return Err(EmbedError::SchemeRejected(value.to_owned()));
    }
    if value.starts_with('/') || value.starts_with('\\') {
        return Err(EmbedError::AbsolutePathRejected(value.to_owned()));
    }
    // MC-003: reject `..` (traversal) as a SUBSTRING ANYWHERE, plus any path separator. This
    // catches `..hidden/secret`, `a..b`, and `../../etc/passwd` — not just a leading `..`.
    if value.contains('/') || value.contains('\\') || value.contains("..") {
        return Err(EmbedError::TraversalRejected(value.to_owned()));
    }
    if value.len() > ASSET_ID_MAX_LENGTH || !is_valid_asset_id_pattern(value) {
        return Err(EmbedError::InvalidRef(value.to_owned()));
    }
    Ok(value.to_owned())
}

/// True for a `C:\…` / `C:/…` drive-letter prefix (mirrors the React `/^[A-Za-z]:[\\/]/`).
fn looks_like_drive_letter(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

/// True when `value` matches the asset-id pattern `^[A-Za-z0-9][A-Za-z0-9._-]*$` (mirrors the
/// React `ASSET_ID_PATTERN`): an opaque id with no separators/colons/spaces.
fn is_valid_asset_id_pattern(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

/// Parse an album/slideshow `ref_value` into its ordered asset-id sequence (mirrors the React
/// `parseAssetRefList`): split on `,`, trim each, drop empties. The returned entries are the
/// RAW (un-validated) ids; the resolver validates each individually so one bad member becomes
/// a per-item error chip while the rest of the sequence still renders.
pub fn parse_asset_ref_list(ref_value: &str) -> Vec<String> {
    ref_value
        .split(',')
        .map(|part| part.trim().to_owned())
        .filter(|part| !part.is_empty())
        .collect()
}

pub fn collection_ref_id(ref_value: &str) -> Option<&str> {
    ref_value
        .trim()
        .strip_prefix(COLLECTION_REF_PREFIX)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// A boxed, `Send` future yielding fetched asset metadata, returned by
/// [`AssetMetadataFetcher::fetch_metadata`]. Spelled out (rather than the `async-trait` macro)
/// so this module adds ZERO new dependency families — the resolution path stays on the crate's
/// existing asynchronous runtime.
pub type MetadataFuture<'a> =
    Pin<Box<dyn Future<Output = Result<EmbedAssetMetadata, EmbedError>> + Send + 'a>>;

/// A boxed, `Send` future yielding fetched asset CONTENT bytes (the raw image/video bytes from
/// `GET /workspaces/{ws}/assets/{id}/content`), returned by [`AssetMetadataFetcher::fetch_content`].
/// These are the bytes the image-embed pipeline decodes off-thread (MC-001) into a texture.
pub type ContentFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<u8>, EmbedError>> + Send + 'a>>;

pub type CollectionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<EmbedCollection, EmbedError>> + Send + 'a>>;

/// The transport an async resolution uses to fetch asset metadata AND content bytes. A trait
/// (rather than a hard `reqwest` call) so the FULL resolution + content-fetch + decode path —
/// the kind/mime check, the caching skip, the typed error mapping, the concurrency cap, the
/// content GET that feeds the off-thread decode — is unit-testable with a COUNTED in-memory mock
/// (AC-9 second-render-no-refetch, MC-002 concurrency, the kind-mismatch test, the
/// content->decode->texture wiring) WITHOUT a backend. The production implementation
/// is supplied by the host.
pub trait AssetMetadataFetcher: Send + Sync {
    fn asset_urls(
        &self,
        base_url: &str,
        workspace_id: &str,
        asset_id: &str,
    ) -> ResolvedAssetUrls;

    /// Fetch the asset metadata for `(workspace_id, asset_id)`. The id is ALREADY validated by
    /// [`validate_asset_ref`] before this is called. Returns the typed metadata or a typed
    /// [`EmbedError`] (NotFound / Forbidden / ServerError / NetworkError).
    fn fetch_metadata<'a>(&'a self, workspace_id: &'a str, asset_id: &'a str)
        -> MetadataFuture<'a>;

    /// Fetch the raw CONTENT bytes for `(workspace_id, asset_id)` (`GET .../content`). These feed
    /// the off-thread image decode (MC-001). The id is ALREADY validated before this is called.
    ///
    /// The DEFAULT impl returns a typed [`EmbedError::MediaLoadFailed`] so a metadata-only mock
    /// (one that only proves the resolution/validation path) does not have to implement content
    /// fetching; the image-content pipeline (and its dedicated mock) overrides this. The
    /// production transport overrides it with a real fetch.
    fn fetch_content<'a>(&'a self, _workspace_id: &'a str, asset_id: &'a str) -> ContentFuture<'a> {
        let asset_id = asset_id.to_owned();
        Box::pin(async move {
            Err(EmbedError::MediaLoadFailed(format!(
                "fetcher does not provide content bytes for asset '{asset_id}'"
            )))
        })
    }

    /// Fetch a bounded presentation tier for a validated media kind. `kind` is part of the
    /// transport contract so production fallback policy cannot accidentally treat a video poster
    /// or an album-grid thumbnail like a single-image thumbnail. The default preserves
    /// compatibility for focused metadata mocks.
    fn fetch_tier<'a>(
        &'a self,
        workspace_id: &'a str,
        asset_id: &'a str,
        _kind: MediaEmbedKind,
        _tier: MediaTier,
    ) -> ContentFuture<'a> {
        self.fetch_content(workspace_id, asset_id)
    }

    /// Resolve backend-owned album/slideshow membership.
    fn fetch_collection<'a>(
        &'a self,
        _workspace_id: &'a str,
        collection_id: &'a str,
    ) -> CollectionFuture<'a> {
        let collection_id = collection_id.to_owned();
        Box::pin(async move {
            Err(EmbedError::ServerError(format!(
                "fetcher does not provide collection '{collection_id}'"
            )))
        })
    }
}

/// Resolve ONE media asset fail-closed: validate the ref, fetch metadata through `fetcher`,
/// check the mime family against `kind`, and build the content/thumbnail URLs. Every failure
/// is a typed [`EmbedError`] — never a panic. This is the native mirror of `resolveEmbedAsset`.
///
/// The base URL is forwarded to the host after metadata validation succeeds.
pub async fn resolve_one(
    kind: MediaEmbedKind,
    workspace_id: &str,
    ref_value: &str,
    base_url: &str,
    fetcher: &dyn AssetMetadataFetcher,
) -> Result<ResolvedAsset, EmbedError> {
    resolve_one_with_timeout(
        kind,
        workspace_id,
        ref_value,
        base_url,
        fetcher,
        EMBED_OPERATION_TIMEOUT,
    )
    .await
}

async fn resolve_one_with_timeout(
    kind: MediaEmbedKind,
    workspace_id: &str,
    ref_value: &str,
    base_url: &str,
    fetcher: &dyn AssetMetadataFetcher,
    operation_timeout: Duration,
) -> Result<ResolvedAsset, EmbedError> {
    if workspace_id.trim().is_empty() {
        return Err(EmbedError::NoWorkspace);
    }
    let asset_id = validate_asset_ref(ref_value)?;
    let metadata = run_with_deadline(
        operation_timeout,
        format!("resolving metadata for asset '{asset_id}'"),
        fetcher.fetch_metadata(workspace_id, &asset_id),
    )
    .await?;
    if metadata.asset_id != asset_id {
        return Err(EmbedError::ServerError(format!(
            "asset metadata id '{}' does not match requested '{asset_id}'",
            metadata.asset_id
        )));
    }
    if metadata.workspace_id != workspace_id {
        return Err(EmbedError::ServerError(format!(
            "asset metadata workspace '{}' does not match requested '{workspace_id}'",
            metadata.workspace_id
        )));
    }
    if metadata.mime.trim().is_empty() {
        return Err(EmbedError::ServerError(format!(
            "asset '{asset_id}' metadata has an empty mime"
        )));
    }
    if !kind.mime_matches(&metadata.mime) {
        return Err(EmbedError::KindMismatch(format!(
            "asset '{asset_id}' is '{}', which does not match the '{}' embed kind",
            metadata.mime,
            kind.ref_kind()
        )));
    }
    let urls = fetcher.asset_urls(base_url, workspace_id, &asset_id);
    Ok(ResolvedAsset {
        content_url: urls.content_url,
        thumbnail_url: urls.thumbnail_url,
        preview_url: urls.preview_url,
        poster_url: urls.poster_url,
        asset: metadata,
    })
}

pub async fn run_with_deadline<T, F>(
    deadline: Duration,
    label: String,
    future: F,
) -> Result<T, EmbedError>
where
    F: Future<Output = Result<T, EmbedError>>,
{
    tokio::time::timeout(deadline, future)
        .await
        .unwrap_or_else(|_| {
            Err(EmbedError::TimedOut(format!(
                "{label} exceeded {deadline:?}"
            )))
        })
}

/// One member of a resolved album/slideshow sequence: its (validated-or-raw) ref and its
/// resolution state. A broken member is a per-item `Err` so the rest of the sequence still
/// renders (fail-closed per item, not all-or-nothing blanking) — mirrors `EmbedSequenceItem`.
#[derive(Debug, Clone)]
pub struct SequenceItem {
    /// The member ref as it appeared in the comma list.
    pub ref_value: String,
    /// This member's resolution outcome.
    pub resolution: Result<ResolvedAsset, EmbedError>,
}

/// Owns the per-member tasks spawned by [`resolve_sequence`]. Dropping the parent resolution
/// future (for example on workspace rebind or timeout) aborts every still-running member instead
/// of detaching background HTTP work from the editor that requested it.
struct SequenceTaskSet(Vec<tokio::task::JoinHandle<SequenceItem>>);

impl Drop for SequenceTaskSet {
    fn drop(&mut self) {
        for task in self.0.drain(..) {
            task.abort();
        }
    }
}

/// Resolve an album/slideshow ordered sequence with a BOUNDED concurrency of
/// [`MAX_CONCURRENT_RESOLUTIONS`] (MC-002): the members resolve in parallel but at most six
/// metadata fetches are in flight at once, gated by a [`tokio::sync::Semaphore`]. An empty
/// list and an oversized list (`> MAX_SEQUENCE_ITEMS`) are themselves typed errors (the
/// caller renders the whole-sequence error chip). Mirrors `resolveEmbedSequence`.
pub async fn resolve_sequence(
    kind: MediaEmbedKind,
    workspace_id: &str,
    ref_value: &str,
    base_url: &str,
    fetcher: Arc<dyn AssetMetadataFetcher>,
) -> Result<Vec<SequenceItem>, EmbedError> {
    resolve_sequence_with_budget(
        kind,
        workspace_id,
        ref_value,
        base_url,
        fetcher,
        Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_RESOLUTIONS)),
    )
    .await
}

/// Runtime variant using the editor's shared media budget. The same six permits are also used by
/// single metadata, tier-body, and decode work, so multiple embeds cannot each create a private
/// six-request fan-out.
pub async fn resolve_sequence_with_budget(
    kind: MediaEmbedKind,
    workspace_id: &str,
    ref_value: &str,
    base_url: &str,
    fetcher: Arc<dyn AssetMetadataFetcher>,
    semaphore: Arc<tokio::sync::Semaphore>,
) -> Result<Vec<SequenceItem>, EmbedError> {
    resolve_sequence_with_budget_and_timeout(
        kind,
        workspace_id,
        ref_value,
        base_url,
        fetcher,
        semaphore,
        EMBED_OPERATION_TIMEOUT,
    )
    .await
}

async fn resolve_sequence_with_budget_and_timeout(
    kind: MediaEmbedKind,
    workspace_id: &str,
    ref_value: &str,
    base_url: &str,
    fetcher: Arc<dyn AssetMetadataFetcher>,
    semaphore: Arc<tokio::sync::Semaphore>,
    operation_timeout: Duration,
) -> Result<Vec<SequenceItem>, EmbedError> {
    run_with_deadline(
        operation_timeout,
        format!("resolving {} sequence", kind.ref_kind()),
        resolve_sequence_with_budget_inner(
            kind,
            workspace_id,
            ref_value,
            base_url,
            fetcher,
            semaphore,
        ),
    )
    .await
}

async fn resolve_sequence_with_budget_inner(
    kind: MediaEmbedKind,
    workspace_id: &str,
    ref_value: &str,
    base_url: &str,
    fetcher: Arc<dyn AssetMetadataFetcher>,
    semaphore: Arc<tokio::sync::Semaphore>,
) -> Result<Vec<SequenceItem>, EmbedError> {
    if workspace_id.trim().is_empty() {
        return Err(EmbedError::NoWorkspace);
    }
    let refs = if let Some(collection_id) = collection_ref_id(ref_value) {
        let collection_id = validate_asset_ref(collection_id)?;
        let _permit = Arc::clone(&semaphore)
            .acquire_owned()
            .await
            .map_err(|_| EmbedError::ServerError("embed work budget closed".to_owned()))?;
        let collection = fetcher
            .fetch_collection(workspace_id, &collection_id)
            .await?;
        if collection.collection_id != collection_id {
            return Err(EmbedError::ServerError(format!(
                "collection response id '{}' does not match requested '{collection_id}'",
                collection.collection_id
            )));
        }
        collection.members
    } else {
        parse_asset_ref_list(ref_value)
    };
    if refs.is_empty() {
        return Err(EmbedError::EmptyRef);
    }
    if refs.len() > MAX_SEQUENCE_ITEMS {
        return Err(EmbedError::InvalidRef(format!(
            "sequence has {} members; the maximum is {MAX_SEQUENCE_ITEMS}",
            refs.len()
        )));
    }

    let mut handles = SequenceTaskSet(Vec::with_capacity(refs.len()));
    for member in refs {
        let sem = Arc::clone(&semaphore);
        let fetcher = Arc::clone(&fetcher);
        let workspace_id = workspace_id.to_owned();
        let base_url = base_url.to_owned();
        handles.0.push(tokio::spawn(async move {
            // Hold a permit for the whole member resolution so at most six run concurrently.
            let _permit = match sem.acquire_owned().await {
                Ok(permit) => permit,
                Err(_) => {
                    return SequenceItem {
                        ref_value: member,
                        resolution: Err(EmbedError::ServerError(
                            "embed work budget closed".to_owned(),
                        )),
                    };
                }
            };
            let resolution =
                resolve_one(kind, &workspace_id, &member, &base_url, fetcher.as_ref()).await;
            SequenceItem {
                ref_value: member,
                resolution,
            }
        }));
    }

    let mut items = Vec::with_capacity(handles.0.len());
    // Await by mutable reference so every handle remains owned by `SequenceTaskSet`; if this
    // parent future is cancelled while one member is awaited, Drop still aborts that member too.
    for handle in &mut handles.0 {
        match handle.await {
            Ok(item) => items.push(item),
            // A spawned member task panicked (should not happen — resolve_one never panics);
            // surface it as a typed server error for that member rather than aborting the set.
            Err(join_err) => items.push(SequenceItem {
                ref_value: String::new(),
                resolution: Err(EmbedError::ServerError(format!(
                    "embed member task failed: {join_err}"
                ))),
            }),
        }
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EmbedResolutionCache;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn test_urls(base_url: &str, workspace_id: &str, asset_id: &str) -> ResolvedAssetUrls {
        let prefix = format!("{base_url}|{workspace_id}|{asset_id}");
        ResolvedAssetUrls {
            content_url: format!("{prefix}|content"),
            thumbnail_url: format!("{prefix}|thumbnail"),
            preview_url: format!("{prefix}|preview"),
            poster_url: format!("{prefix}|poster"),
        }
    }

    // ── A counted mock fetcher (no backend) used by the resolution / caching / concurrency tests ─

    /// A mock metadata fetcher that COUNTS calls (AC-9) and can simulate slow fetches +
    /// concurrency tracking (MC-002). It NEVER touches the network.
    struct MockFetcher {
        calls: AtomicUsize,
        in_flight: AtomicUsize,
        max_in_flight: AtomicUsize,
        delay_ms: u64,
        mime: String,
    }

    impl MockFetcher {
        fn new(mime: &str, delay_ms: u64) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                in_flight: AtomicUsize::new(0),
                max_in_flight: AtomicUsize::new(0),
                delay_ms,
                mime: mime.to_owned(),
            }
        }
        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
        fn observed_max_in_flight(&self) -> usize {
            self.max_in_flight.load(Ordering::SeqCst)
        }
    }

    impl AssetMetadataFetcher for MockFetcher {
        fn asset_urls(&self, base_url: &str, workspace_id: &str, asset_id: &str) -> ResolvedAssetUrls {
            test_urls(base_url, workspace_id, asset_id)
        }

        fn fetch_metadata<'a>(
            &'a self,
            workspace_id: &'a str,
            asset_id: &'a str,
        ) -> MetadataFuture<'a> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                // Track the high-water mark of simultaneous in-flight fetches (MC-002 proof).
                self.max_in_flight.fetch_max(now, Ordering::SeqCst);
                if self.delay_ms > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
                }
                self.in_flight.fetch_sub(1, Ordering::SeqCst);
                Ok(EmbedAssetMetadata {
                    asset_id: asset_id.to_owned(),
                    workspace_id: workspace_id.to_owned(),
                    kind: "image".to_owned(),
                    mime: self.mime.clone(),
                    original_filename: Some(format!("{asset_id}.png")),
                    content_hash: "deadbeef".to_owned(),
                    size_bytes: 1024,
                    width: Some(640),
                    height: Some(480),
                })
            })
        }
    }

    struct PendingMetadataFetcher;

    impl AssetMetadataFetcher for PendingMetadataFetcher {
        fn asset_urls(&self, base_url: &str, workspace_id: &str, asset_id: &str) -> ResolvedAssetUrls {
            test_urls(base_url, workspace_id, asset_id)
        }

        fn fetch_metadata<'a>(
            &'a self,
            _workspace_id: &'a str,
            _asset_id: &'a str,
        ) -> MetadataFuture<'a> {
            Box::pin(std::future::pending())
        }
    }

    #[tokio::test]
    async fn direct_resolve_one_path_has_a_hard_deadline() {
        let started = std::time::Instant::now();
        let error = resolve_one_with_timeout(
            MediaEmbedKind::Images,
            "ws",
            "hung",
            "http://b",
            &PendingMetadataFetcher,
            Duration::from_millis(5),
        )
        .await
        .expect_err("a direct resolver call may not wait forever");
        assert_eq!(error.kind_str(), "timed_out");
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn direct_sequence_budget_wait_has_a_hard_deadline() {
        let fetcher: Arc<dyn AssetMetadataFetcher> = Arc::new(PendingMetadataFetcher);
        let started = std::time::Instant::now();
        let error = resolve_sequence_with_budget_and_timeout(
            MediaEmbedKind::Album,
            "ws",
            "one",
            "http://b",
            fetcher,
            Arc::new(tokio::sync::Semaphore::new(0)),
            Duration::from_millis(5),
        )
        .await
        .expect_err("a direct sequence semaphore wait may not outlive the operation budget");
        assert_eq!(error.kind_str(), "timed_out");
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn resolve_one_validates_before_any_fetch_ac3() {
        // AC-3: a `..` ref is rejected with TraversalRejected BEFORE any fetch is issued.
        let fetcher = MockFetcher::new("image/png", 0);
        let err = resolve_one(
            MediaEmbedKind::Images,
            "ws",
            "../secret",
            "http://b",
            &fetcher,
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind_str(), "traversal_rejected");
        assert_eq!(
            fetcher.call_count(),
            0,
            "AC-3: NO HTTP call may be made for a rejected ref"
        );
    }

    #[tokio::test]
    async fn resolve_one_rejects_scheme_before_fetch_ac4() {
        // AC-4: an http:// ref is SchemeRejected with no fetch.
        let fetcher = MockFetcher::new("image/png", 0);
        let err = resolve_one(
            MediaEmbedKind::Images,
            "ws",
            "http://evil/x",
            "http://b",
            &fetcher,
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind_str(), "scheme_rejected");
        assert_eq!(fetcher.call_count(), 0);
    }

    #[tokio::test]
    async fn resolve_one_ok_builds_urls() {
        let fetcher = MockFetcher::new("image/png", 0);
        let resolved = resolve_one(
            MediaEmbedKind::Images,
            "ws1",
            "a1",
            "http://127.0.0.1:37501",
            &fetcher,
        )
        .await
        .unwrap();
        assert_eq!(resolved.asset.asset_id, "a1");
        assert_eq!(
            resolved.content_url,
            "http://127.0.0.1:37501|ws1|a1|content"
        );
        assert_eq!(
            resolved.thumbnail_url,
            "http://127.0.0.1:37501|ws1|a1|thumbnail"
        );
        assert_eq!(fetcher.call_count(), 1);
    }

    #[tokio::test]
    async fn resolve_one_kind_mismatch_is_fail_closed() {
        // A video asset inside an `images` embed -> kind_mismatch (fetch happened, mime checked).
        let fetcher = MockFetcher::new("video/mp4", 0);
        let err = resolve_one(MediaEmbedKind::Images, "ws", "a1", "http://b", &fetcher)
            .await
            .unwrap_err();
        assert_eq!(err.kind_str(), "kind_mismatch");
    }

    #[tokio::test]
    async fn resolve_one_does_not_reject_large_original_before_bounded_tier_fetch() {
        struct OversizedFetcher;
        impl AssetMetadataFetcher for OversizedFetcher {
        fn asset_urls(&self, base_url: &str, workspace_id: &str, asset_id: &str) -> ResolvedAssetUrls {
            test_urls(base_url, workspace_id, asset_id)
        }

            fn fetch_metadata<'a>(
                &'a self,
                workspace_id: &'a str,
                asset_id: &'a str,
            ) -> MetadataFuture<'a> {
                Box::pin(async move {
                    Ok(EmbedAssetMetadata {
                        asset_id: asset_id.to_owned(),
                        workspace_id: workspace_id.to_owned(),
                        kind: "image".to_owned(),
                        mime: "image/png".to_owned(),
                        original_filename: None,
                        content_hash: String::new(),
                        size_bytes: (MAX_FULL_IMAGE_BYTES + 1) as u64,
                        width: Some(1),
                        height: Some(1),
                    })
                })
            }
        }

        let resolved = resolve_one(
            MediaEmbedKind::Images,
            "ws",
            "too-large",
            "http://b",
            &OversizedFetcher,
        )
        .await
        .expect("metadata size must not block a separately bounded thumbnail fetch");
        assert_eq!(resolved.asset.size_bytes, (MAX_FULL_IMAGE_BYTES + 1) as u64);
        assert!(resolved.thumbnail_url.ends_with("|thumbnail"));
    }

    #[tokio::test]
    async fn no_workspace_is_rejected_before_fetch() {
        let fetcher = MockFetcher::new("image/png", 0);
        let err = resolve_one(MediaEmbedKind::Images, "  ", "a1", "http://b", &fetcher)
            .await
            .unwrap_err();
        assert_eq!(err.kind_str(), "no_workspace");
        assert_eq!(fetcher.call_count(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sequence_concurrency_capped_at_six_mc002() {
        // MC-002: 12 members, each fetch sleeps 30ms; at most MAX_CONCURRENT_RESOLUTIONS (6) may
        // be in flight at any instant. The mock tracks the high-water in-flight count.
        let refs: Vec<String> = (0..12).map(|i| format!("a{i}")).collect();
        let ref_value = refs.join(",");
        let fetcher: Arc<MockFetcher> = Arc::new(MockFetcher::new("image/png", 30));
        let fetcher_dyn: Arc<dyn AssetMetadataFetcher> = fetcher.clone();
        let items = resolve_sequence(
            MediaEmbedKind::Album,
            "ws",
            &ref_value,
            "http://b",
            fetcher_dyn,
        )
        .await
        .unwrap();
        assert_eq!(items.len(), 12, "all 12 members resolved");
        assert_eq!(fetcher.call_count(), 12);
        assert!(
            fetcher.observed_max_in_flight() <= MAX_CONCURRENT_RESOLUTIONS,
            "MC-002: at most {MAX_CONCURRENT_RESOLUTIONS} concurrent fetches; observed max {}",
            fetcher.observed_max_in_flight()
        );
    }

    #[tokio::test]
    async fn sequence_per_item_failclosed() {
        // A 3-member sequence where the middle ref is a traversal: the bad member is a per-item
        // Err while the other two resolve OK (not all-or-nothing).
        let fetcher: Arc<dyn AssetMetadataFetcher> = Arc::new(MockFetcher::new("image/png", 0));
        let items = resolve_sequence(
            MediaEmbedKind::Slideshow,
            "ws",
            "a1, ../bad , a3",
            "http://b",
            fetcher,
        )
        .await
        .unwrap();
        assert_eq!(items.len(), 3);
        assert!(items[0].resolution.is_ok());
        assert_eq!(
            items[1].resolution.as_ref().unwrap_err().kind_str(),
            "traversal_rejected"
        );
        assert!(items[2].resolution.is_ok());
    }

    #[tokio::test]
    async fn backend_collection_owns_sequence_membership_and_order() {
        struct CollectionFetcher {
            metadata_calls: std::sync::Mutex<Vec<String>>,
        }
        impl AssetMetadataFetcher for CollectionFetcher {
        fn asset_urls(&self, base_url: &str, workspace_id: &str, asset_id: &str) -> ResolvedAssetUrls {
            test_urls(base_url, workspace_id, asset_id)
        }

            fn fetch_metadata<'a>(
                &'a self,
                workspace_id: &'a str,
                asset_id: &'a str,
            ) -> MetadataFuture<'a> {
                Box::pin(async move {
                    self.metadata_calls
                        .lock()
                        .unwrap()
                        .push(asset_id.to_owned());
                    Ok(EmbedAssetMetadata {
                        asset_id: asset_id.to_owned(),
                        workspace_id: workspace_id.to_owned(),
                        kind: "image".to_owned(),
                        mime: "image/png".to_owned(),
                        original_filename: None,
                        content_hash: String::new(),
                        size_bytes: 16,
                        width: Some(1),
                        height: Some(1),
                    })
                })
            }

            fn fetch_collection<'a>(
                &'a self,
                _workspace_id: &'a str,
                collection_id: &'a str,
            ) -> CollectionFuture<'a> {
                Box::pin(async move {
                    Ok(EmbedCollection {
                        collection_id: collection_id.to_owned(),
                        title: Some("Backend order".to_owned()),
                        members: vec!["third".to_owned(), "first".to_owned(), "second".to_owned()],
                    })
                })
            }
        }

        let fetcher = Arc::new(CollectionFetcher {
            metadata_calls: std::sync::Mutex::new(Vec::new()),
        });
        let fetcher_dyn: Arc<dyn AssetMetadataFetcher> = fetcher.clone();
        let items = resolve_sequence(
            MediaEmbedKind::Album,
            "ws",
            "collection:ordered-set",
            "http://b",
            fetcher_dyn,
        )
        .await
        .unwrap();
        assert_eq!(
            items
                .iter()
                .map(|item| item.ref_value.as_str())
                .collect::<Vec<_>>(),
            vec!["third", "first", "second"]
        );
        let mut calls = fetcher.metadata_calls.lock().unwrap().clone();
        calls.sort();
        assert_eq!(calls, vec!["first", "second", "third"]);
    }

    #[tokio::test]
    async fn empty_and_oversized_sequences_are_typed_errors() {
        let fetcher: Arc<dyn AssetMetadataFetcher> = Arc::new(MockFetcher::new("image/png", 0));
        assert_eq!(
            resolve_sequence(
                MediaEmbedKind::Album,
                "ws",
                "  ,  ",
                "http://b",
                Arc::clone(&fetcher)
            )
            .await
            .unwrap_err()
            .kind_str(),
            "empty_ref"
        );
        let huge: String = (0..(MAX_SEQUENCE_ITEMS + 1))
            .map(|i| format!("a{i}"))
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(
            resolve_sequence(MediaEmbedKind::Album, "ws", &huge, "http://b", fetcher)
                .await
                .unwrap_err()
                .kind_str(),
            "invalid_ref"
        );
    }

    // ── AC-9: the resolution cache skips a second fetch for a terminal asset ──────────────────

    #[test]
    fn cache_needs_fetch_only_when_absent() {
        let mut cache = EmbedResolutionCache::new();
        assert!(cache.needs_fetch("a1"), "absent -> needs fetch");
        cache.insert("a1", EmbedResolutionState::Resolving);
        assert!(
            !cache.needs_fetch("a1"),
            "resolving -> in flight, do not re-spawn"
        );
        assert!(cache.is_resolving("a1"));
        cache.insert(
            "a1",
            EmbedResolutionState::Ok(ResolvedAsset {
                asset: EmbedAssetMetadata {
                    asset_id: "a1".into(),
                    workspace_id: "ws".into(),
                    kind: "image".into(),
                    mime: "image/png".into(),
                    original_filename: None,
                    content_hash: String::new(),
                    size_bytes: 0,
                    width: None,
                    height: None,
                },
                content_url: "u".into(),
                thumbnail_url: "t".into(),
                preview_url: "p".into(),
                poster_url: "v".into(),
            }),
        );
        assert!(
            !cache.needs_fetch("a1"),
            "AC-9: a resolved (Ok) asset is NEVER re-fetched"
        );
        cache.insert(
            "a2",
            EmbedResolutionState::Err(EmbedError::NotFound("a2".into())),
        );
        assert!(
            !cache.needs_fetch("a2"),
            "AC-9: a failed (Err) asset is NEVER re-fetched"
        );
        assert_eq!(cache.len(), 2);
    }

    /// AC-9 end-to-end with the COUNTED mock: the renderer-shaped "fetch-once" loop spawns a
    /// fetch only when `needs_fetch`, marks `Resolving`, then stores the terminal state — a
    /// second pass issues NO second call.
    #[tokio::test]
    async fn second_render_issues_no_second_fetch_ac9() {
        let fetcher = MockFetcher::new("image/png", 0);
        let mut cache = EmbedResolutionCache::new();

        // Pass 1: not cached -> fetch once, store Ok.
        async fn render_pass(cache: &mut EmbedResolutionCache, fetcher: &MockFetcher) {
            let asset_id = "a1";
            if cache.needs_fetch(asset_id) {
                cache.insert(asset_id, EmbedResolutionState::Resolving);
                let res =
                    resolve_one(MediaEmbedKind::Images, "ws", asset_id, "http://b", fetcher).await;
                cache.insert(
                    asset_id,
                    match res {
                        Ok(r) => EmbedResolutionState::Ok(r),
                        Err(e) => EmbedResolutionState::Err(e),
                    },
                );
            }
        }
        render_pass(&mut cache, &fetcher).await;
        assert_eq!(fetcher.call_count(), 1, "first render fetches once");
        // Pass 2: cached terminal -> NO fetch.
        render_pass(&mut cache, &fetcher).await;
        assert_eq!(
            fetcher.call_count(),
            1,
            "AC-9: second render issues NO second fetch"
        );
    }
}
