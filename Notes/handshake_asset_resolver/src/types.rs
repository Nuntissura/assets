use std::time::Duration;
use thiserror::Error;

/// The `hsLink` ref kinds that render as media embeds (mirrors the React
/// `MEDIA_EMBED_REF_KINDS`). Note `images` (plural) is the IMAGE kind — the backend
/// `RichDocEmbed.ref_kind` value is `"images"`, NOT `"image"` (a frequent transcription
/// slip the contract's scope text makes; the REAL backend shape, verified against
/// `embed_assets.ts`, is `images`).
pub const MEDIA_EMBED_REF_KINDS: [&str; 4] = ["images", "video", "album", "slideshow"];

/// DoS guard (mirrors the React `MAX_SEQUENCE_ITEMS`): an album/slideshow ref-list caps at
/// this many members. A hostile/corrupt document could otherwise carry thousands of
/// comma-separated ids, fanning out one metadata request each.
pub const MAX_SEQUENCE_ITEMS: usize = 100;

/// Concurrency cap for album/slideshow sequence resolution (red-team RISK-2 / MC-002): at
/// most this many member metadata fetches run at once, via a [`tokio::sync::Semaphore`], so
/// a 50-thumbnail album never opens 50 simultaneous backend connections.
pub const MAX_CONCURRENT_RESOLUTIONS: usize = 6;

/// Bounded transport/decode limits. These reject oversized bodies and decompression bombs with a
/// typed visible error before an allocation can grow without bound.
pub const MAX_METADATA_BYTES: usize = 1024 * 1024;
pub const MAX_THUMBNAIL_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_POSTER_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_PREVIEW_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_FULL_IMAGE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_IMAGE_DIMENSION: u32 = 16_384;
pub const MAX_IMAGE_PIXELS: u64 = 80_000_000;

/// Hard transport/operation deadlines for every production embed request. The request timeout
/// covers response-body streaming as well as the initial response, while the shorter connect
/// timeout prevents an unreachable host from consuming the entire operation budget.
pub const EMBED_HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
pub const EMBED_OPERATION_TIMEOUT: Duration = Duration::from_secs(15);

/// The longest an asset id may be (mirrors `ASSET_ID_MAX_LENGTH`).
pub(crate) const ASSET_ID_MAX_LENGTH: usize = 256;

/// The media family an embed kind expects, so a kind/mime mismatch is fail-closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaEmbedKind {
    /// `images`: a single still image.
    Images,
    /// `video`: a video asset (poster + play affordance; no in-process decode).
    Video,
    /// `album`: an ordered grid of images.
    Album,
    /// `slideshow`: an ordered one-at-a-time image sequence.
    Slideshow,
}

/// The byte tier a view requests. Grid cells use `Thumbnail`, video uses `Poster`, and only an
/// active slideshow frame or open modal requests `Full`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaTier {
    Thumbnail,
    Preview,
    Poster,
    Full,
}

impl MediaTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Thumbnail => "thumb",
            Self::Preview => "preview",
            Self::Poster => "poster",
            Self::Full => "full",
        }
    }

    pub fn max_body_bytes(self) -> usize {
        match self {
            Self::Thumbnail => MAX_THUMBNAIL_BYTES,
            Self::Preview => MAX_PREVIEW_BYTES,
            Self::Poster => MAX_POSTER_BYTES,
            Self::Full => MAX_FULL_IMAGE_BYTES,
        }
    }
}

impl MediaEmbedKind {
    /// Parse a backend `ref_kind` string into a media kind, or `None` for a non-media kind
    /// (ordinary wikilink kinds keep the MT-015 chip rendering, not handled here).
    pub fn from_ref_kind(ref_kind: &str) -> Option<Self> {
        match ref_kind {
            "images" => Some(Self::Images),
            "video" => Some(Self::Video),
            "album" => Some(Self::Album),
            "slideshow" => Some(Self::Slideshow),
            _ => None,
        }
    }

    /// The backend `ref_kind` string for this kind (the inverse of [`Self::from_ref_kind`]).
    pub fn ref_kind(self) -> &'static str {
        match self {
            Self::Images => "images",
            Self::Video => "video",
            Self::Album => "album",
            Self::Slideshow => "slideshow",
        }
    }

    /// True when this kind resolves an ORDERED SEQUENCE (album/slideshow) rather than a single
    /// asset; such a `ref_value` is a comma-separated asset-id list.
    pub fn is_sequence(self) -> bool {
        matches!(self, Self::Album | Self::Slideshow)
    }

    /// True when `mime` matches the media family this kind expects (mirrors
    /// `mimeMatchesEmbedKind`). Fail-closed: a video asset inside an `images` embed is a
    /// [`EmbedError::KindMismatch`].
    pub fn mime_matches(self, mime: &str) -> bool {
        let normalized = mime.to_ascii_lowercase();
        match self {
            Self::Video => normalized.starts_with("video/"),
            Self::Images | Self::Album | Self::Slideshow => normalized.starts_with("image/"),
        }
    }
}

/// The typed reasons an embed cannot resolve. Every variant renders as a VISIBLE chip
/// (fail-closed). The kebab-case [`Self::kind_str`] matches the React `EmbedErrorKind`
/// vocabulary verbatim, so the native chip text is identical to the web app's.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EmbedError {
    /// The editor has no workspace bound; an embed cannot resolve a workspace asset.
    #[error("no workspace context: media embeds resolve workspace assets and need a workspace id")]
    NoWorkspace,
    /// The ref value is empty / whitespace only.
    #[error("embed reference is empty")]
    EmptyRef,
    /// An absolute / UNC / drive-letter path was supplied where an opaque asset id is required.
    #[error("absolute path '{0}' is forbidden: embeds are asset ids, never machine-local paths")]
    AbsolutePathRejected(String),
    /// A `..` traversal or path separator appeared in the asset id.
    #[error("'{0}' contains path separators or traversal; embeds are opaque asset ids")]
    TraversalRejected(String),
    /// An http(s)/file/javascript/data scheme appeared in the asset ref.
    #[error("'{0}' carries a scheme; media embeds resolve workspace asset ids only")]
    SchemeRejected(String),
    /// The asset id is otherwise malformed (too long, illegal characters).
    #[error("'{0}' is not a valid asset id")]
    InvalidRef(String),
    /// The backend returned 404 for the asset.
    #[error("asset not found: {0}")]
    NotFound(String),
    /// The backend returned 401/403 for the asset.
    #[error("asset is not accessible: {0}")]
    Forbidden(String),
    /// The backend returned 5xx or a malformed metadata body.
    #[error("server error: {0}")]
    ServerError(String),
    /// The fetch itself failed (backend unreachable / transport error).
    #[error("network error: {0}")]
    NetworkError(String),
    /// The asset mime does not match the embed kind (e.g. a video asset in an `images` embed).
    #[error("kind mismatch: {0}")]
    KindMismatch(String),
    /// The decoder could not decode/play the resolved bytes (corrupt/unsupported media).
    #[error("media load failed: {0}")]
    MediaLoadFailed(String),
    /// A backend fetch or decode did not finish inside the bounded embed deadline.
    #[error("embed operation timed out: {0}")]
    TimedOut(String),
    /// A response body or decoded image exceeded the explicit media safety bounds.
    #[error("embed resource limit exceeded: {0}")]
    ResourceLimit(String),
}

impl EmbedError {
    /// The kebab-case kind string (verbatim from the React `EmbedErrorKind`). This is the
    /// stable text the error chip shows and the AccessKit label carries, so an out-of-process
    /// agent reads the SAME error vocabulary the web app used.
    pub fn kind_str(&self) -> &'static str {
        match self {
            EmbedError::NoWorkspace => "no_workspace",
            EmbedError::EmptyRef => "empty_ref",
            EmbedError::AbsolutePathRejected(_) => "absolute_path_rejected",
            EmbedError::TraversalRejected(_) => "traversal_rejected",
            EmbedError::SchemeRejected(_) => "scheme_rejected",
            EmbedError::InvalidRef(_) => "invalid_ref",
            EmbedError::NotFound(_) => "not_found",
            EmbedError::Forbidden(_) => "forbidden",
            EmbedError::ServerError(_) => "server_error",
            EmbedError::NetworkError(_) => "network_error",
            EmbedError::KindMismatch(_) => "kind_mismatch",
            EmbedError::MediaLoadFailed(_) => "media_load_failed",
            EmbedError::TimedOut(_) => "timed_out",
            EmbedError::ResourceLimit(_) => "resource_limit",
        }
    }
}

/// Backend asset metadata (the native mirror of the React `EmbedAssetMetadata`, which itself
/// mirrors the backend `storage/loom.rs` `Asset` row). Only the fields the views need are
/// modeled; unknown fields are ignored by serde so a forward-compatible backend body still
/// deserializes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct EmbedAssetMetadata {
    pub asset_id: String,
    pub workspace_id: String,
    #[serde(default)]
    pub kind: String,
    pub mime: String,
    #[serde(default)]
    pub original_filename: Option<String>,
    #[serde(default)]
    pub content_hash: String,
    #[serde(default)]
    pub size_bytes: u64,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

/// A fully-resolved single asset: its metadata plus the content/thumbnail URLs the view
/// loads (mirrors the React `EmbedResolution` ok branch). The URLs are constructed from the
/// host transport after metadata validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAsset {
    /// The backend asset metadata.
    pub asset: EmbedAssetMetadata,
    /// `GET /workspaces/{ws}/assets/{id}/content` — full-res original bytes.
    pub content_url: String,
    /// `GET /workspaces/{ws}/assets/{id}/thumbnail` — thumbnail bytes (grid/sequence first load).
    pub thumbnail_url: String,
    /// Mid-resolution tier used by an enlarged preview before/while full pixels are available.
    pub preview_url: String,
    /// Video poster tier; this is fetched without loading the full video body.
    pub poster_url: String,
}

/// Backend-owned ordered album/slideshow membership (`collection:<id>` parity).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct EmbedCollection {
    pub collection_id: String,
    #[serde(default)]
    pub title: Option<String>,
    pub members: Vec<String>,
}

pub const COLLECTION_REF_PREFIX: &str = "collection:";

/// The resolution state of ONE embed target, cached per asset id so a repeated render does
/// not re-fetch (AC-9). This is the value stored in the per-editor resolution cache.
#[derive(Debug, Clone)]
// The resolved asset stays inline so steady-state rendering avoids an extra allocation and indirection.
#[allow(clippy::large_enum_variant)]
pub enum EmbedResolutionState {
    /// The fetch is in flight (the view shows an `egui::Spinner`).
    Resolving,
    /// Resolved OK — the view renders the media.
    Ok(ResolvedAsset),
    /// Resolution failed with a typed error — the view renders the error chip (never blank).
    Err(EmbedError),
}

impl EmbedResolutionState {
    /// True when this state is terminal (Ok or Err) — a terminal state is NOT re-fetched
    /// (AC-9 caching: the resolver skips an asset that already resolved or failed).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            EmbedResolutionState::Ok(_) | EmbedResolutionState::Err(_)
        )
    }
}

/// URLs supplied by the host after metadata validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAssetUrls {
    pub content_url: String,
    pub thumbnail_url: String,
    pub preview_url: String,
    pub poster_url: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{validate_asset_ref, parse_asset_ref_list};

    #[test]
    fn empty_ref_is_rejected() {
        assert_eq!(validate_asset_ref(""), Err(EmbedError::EmptyRef));
        assert_eq!(validate_asset_ref("   "), Err(EmbedError::EmptyRef));
        assert_eq!(EmbedError::EmptyRef.kind_str(), "empty_ref");
    }

    #[test]
    fn traversal_dotdot_rejected_anywhere_mc003() {
        // MC-003: `..` ANYWHERE in the string is rejected, not just a leading path component.
        for bad in [
            "..",
            "../../etc/passwd",
            "..hidden",
            "a..b",
            "secret..",
            "foo/../bar",
        ] {
            let err = validate_asset_ref(bad).unwrap_err();
            assert_eq!(
                err.kind_str(),
                "traversal_rejected",
                "ref '{bad}' must be traversal_rejected, got {err:?}"
            );
        }
    }

    #[test]
    fn path_separators_rejected() {
        for bad in ["dir/asset", "dir\\asset", "a/b/c"] {
            assert_eq!(
                validate_asset_ref(bad).unwrap_err().kind_str(),
                "traversal_rejected"
            );
        }
    }

    #[test]
    fn absolute_paths_rejected() {
        // Leading slash / backslash and drive letters.
        assert_eq!(
            validate_asset_ref("/etc/passwd").unwrap_err().kind_str(),
            "absolute_path_rejected"
        );
        assert_eq!(
            validate_asset_ref("\\\\unc\\share").unwrap_err().kind_str(),
            "absolute_path_rejected"
        );
        assert_eq!(
            validate_asset_ref("C:\\Windows").unwrap_err().kind_str(),
            "absolute_path_rejected"
        );
        assert_eq!(
            validate_asset_ref("D:/x").unwrap_err().kind_str(),
            "absolute_path_rejected"
        );
    }

    #[test]
    fn schemes_rejected() {
        for bad in [
            "http://evil.test/x",
            "https://x",
            "file:///etc",
            "javascript:alert(1)",
            "data:text/html",
        ] {
            assert_eq!(
                validate_asset_ref(bad).unwrap_err().kind_str(),
                "scheme_rejected",
                "ref '{bad}' must be scheme_rejected"
            );
        }
    }

    #[test]
    fn valid_asset_ids_accept_and_trim() {
        assert_eq!(validate_asset_ref("asset123").unwrap(), "asset123");
        assert_eq!(validate_asset_ref("  a-b_c.d  ").unwrap(), "a-b_c.d");
        // Over-length is invalid_ref.
        let long = "a".repeat(ASSET_ID_MAX_LENGTH + 1);
        assert_eq!(
            validate_asset_ref(&long).unwrap_err().kind_str(),
            "invalid_ref"
        );
        // Illegal characters (space) -> invalid_ref.
        assert_eq!(
            validate_asset_ref("a b").unwrap_err().kind_str(),
            "invalid_ref"
        );
    }

    #[test]
    fn parse_ref_list_splits_trims_and_drops_empties() {
        assert_eq!(parse_asset_ref_list("a, b ,c"), vec!["a", "b", "c"]);
        assert_eq!(parse_asset_ref_list(" , a , , b , "), vec!["a", "b"]);
        assert!(parse_asset_ref_list("   ").is_empty());
    }

    #[test]
    fn mime_matches_kind() {
        assert!(MediaEmbedKind::Images.mime_matches("image/png"));
        assert!(MediaEmbedKind::Images.mime_matches("IMAGE/JPEG"));
        assert!(!MediaEmbedKind::Images.mime_matches("video/mp4"));
        assert!(MediaEmbedKind::Video.mime_matches("video/webm"));
        assert!(!MediaEmbedKind::Video.mime_matches("image/png"));
        assert!(MediaEmbedKind::Album.mime_matches("image/gif"));
        assert!(MediaEmbedKind::Slideshow.mime_matches("image/webp"));
    }

    #[test]
    fn ref_kind_round_trips() {
        for k in [
            MediaEmbedKind::Images,
            MediaEmbedKind::Video,
            MediaEmbedKind::Album,
            MediaEmbedKind::Slideshow,
        ] {
            assert_eq!(MediaEmbedKind::from_ref_kind(k.ref_kind()), Some(k));
        }
        assert_eq!(MediaEmbedKind::from_ref_kind("wp"), None);
        assert_eq!(MediaEmbedKind::from_ref_kind("note"), None);
        // The contract scope text's "image" (singular) is NOT a media kind — only "images".
        assert_eq!(MediaEmbedKind::from_ref_kind("image"), None);
    }

}
