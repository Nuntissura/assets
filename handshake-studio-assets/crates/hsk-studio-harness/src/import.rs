//! Open directly / convert to native (SMB-CONVERT-NATIVE CN-3; staged spec STU-IO-191): one flow
//! `source bytes -> sniff -> importer -> ImportedDocument -> package bytes -> Session::open_bytes`.
//!
//! Each importer is an explicitly selected provider behind its own feature (no default features):
//! `import-psd` (PSD/PSB through `hsk-studio-interop-psd`), `import-raw` (DNG and the camera raw
//! containers `hsk-studio-develop` decodes) and `import-tiff` (plain TIFF through
//! `hsk-studio-interop-tiff`, SMB-CONVERT-TIFF), `import-pdf` (PDF through `hsk-studio-press`:
//! carried, not imported; module `pdf`). A detected format whose importer is not in this build is
//! the typed refusal `format_unavailable` naming the format, the feature and the reason; unknown
//! bytes are `format_unrecognised`. Nothing is reported as imported that was not (tests in `tests/import_psd.rs`).
//!
//! The converted package carries, next to the document and its digest-keyed tile assets, the typed
//! import entries of `hsk-studio-package` (CN-2): the importer's standalone records (when it produced
//! any), the import receipt (loss report, provenance, PSD layer map and layer extras) and the original
//! source bytes, so nothing the importer did not map is lost (STU-IO-001/005).
//!
//! Colour (SMB-CONVERT-COLOUR, module `colour`): every importer passes a Prism-backed converter, so an
//! embedded profile is converted into the linear-light sRGB working space and the receipt key
//! `colour` records the route; a missing or unusable profile is a typed loss entry.
//!
//! Limits of this slice (recorded in every receipt's `not_proven`): identities come from
//! [`SourceMint`], a deterministic standalone stand-in seeded by the source digest, not a host
//! `IdMint`.
#![deny(clippy::arithmetic_side_effects)]

/// A source format recognised by its leading bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceFormat {
    Psd,
    Psb,
    /// A TIFF or BigTIFF container (including DNG, NEF and ARW, which are TIFF-based; the camera-raw
    /// route will decide from the IFDs, not from the magic).
    Tiff,
    /// A camera raw container that is not plain TIFF by magic (CR2, CR3, ORF, RW2, RAF).
    CameraRaw,
    Pdf,
    /// PNG (SMB-CONVERT-RASTER; importer `import-raster` through `hsk-studio-interop-raster`).
    Png,
    /// JPEG (`FF D8 FF`; importer `import-raster`).
    Jpeg,
    /// WebP (`RIFF....WEBP`): recognised; interop-raster refuses `webp_decoder_unavailable`.
    Webp,
}

impl SourceFormat {
    pub fn name(self) -> &'static str {
        match self {
            Self::Psd => "psd",
            Self::Psb => "psb",
            Self::Tiff => "tiff",
            Self::CameraRaw => "camera_raw",
            Self::Pdf => "pdf",
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Webp => "webp",
        }
    }

    /// The harness feature that selects this format's importer.
    pub fn feature(self) -> &'static str {
        match self {
            Self::Psd | Self::Psb => "import-psd",
            Self::Tiff => "import-tiff",
            Self::CameraRaw => "import-raw",
            Self::Pdf => "import-pdf",
            Self::Png | Self::Jpeg | Self::Webp => "import-raster",
        }
    }
}

/// Recognises a source by its magic bytes; `None` when nothing matches.
pub fn sniff(bytes: &[u8]) -> Option<SourceFormat> {
    let at = |offset: usize, magic: &[u8]| {
        offset.checked_add(magic.len()).and_then(|end| bytes.get(offset..end)) == Some(magic)
    };
    if at(0, b"8BPS") {
        return match bytes.get(4..6) {
            Some([0, 1]) => Some(SourceFormat::Psd),
            Some([0, 2]) => Some(SourceFormat::Psb),
            _ => None,
        };
    }
    if at(0, b"%PDF-") {
        return Some(SourceFormat::Pdf);
    }
    // Raster interchange magics (PNG signature, JPEG SOI + marker, RIFF/WEBP) never collide with TIFF.
    if at(0, &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some(SourceFormat::Png);
    }
    if at(0, &[0xFF, 0xD8, 0xFF]) {
        return Some(SourceFormat::Jpeg);
    }
    if at(0, b"RIFF") && at(8, b"WEBP") {
        return Some(SourceFormat::Webp);
    }
    // CR2 is a TIFF container that names itself at byte 8; CR3 is ISO-BMFF with brand `crx `.
    if (at(0, b"II*\0") && at(8, b"CR\x02")) || at(4, b"ftypcrx") {
        return Some(SourceFormat::CameraRaw);
    }
    if at(0, b"IIRO") || at(0, b"IIRS") || at(0, b"IIU\0") || at(0, b"FUJIFILMCCD-RAW") {
        return Some(SourceFormat::CameraRaw);
    }
    if at(0, b"II*\0") || at(0, b"MM\0*") || at(0, b"II+\0") || at(0, b"MM\0+") {
        return Some(SourceFormat::Tiff);
    }
    None
}

/// Why an import produced no document. Codes are stable snake_case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportError {
    /// No known magic.
    FormatUnrecognised,
    /// Detected, but this build has no importer for it (`reason` says whether the feature is off or
    /// the importer does not exist yet).
    FormatUnavailable { format: SourceFormat, feature: &'static str, reason: &'static str },
    /// The importer refused the source (its own typed code, passed through unchanged).
    Importer { format: SourceFormat, code: &'static str },
    /// Writing the native container failed (package code).
    Package(&'static str),
    Canceled,
}

impl ImportError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::FormatUnrecognised => "format_unrecognised",
            Self::FormatUnavailable { .. } => "format_unavailable",
            Self::Importer { code, .. } => code,
            Self::Package(code) => code,
            Self::Canceled => "canceled",
        }
    }

    /// Machine-readable detail for reports.
    pub fn detail(&self) -> serde_json::Value {
        match self {
            Self::FormatUnavailable { format, feature, reason } => {
                serde_json::json!({"format":format.name(),"feature":feature,"reason":reason})
            }
            Self::Importer { format, code } => serde_json::json!({"format":format.name(),"importer_code":code}),
            _ => serde_json::json!({}),
        }
    }
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for ImportError {}

/// Largest source the harness reads (equal to the PSD importer's and the package's entry bound).
pub const MAX_SOURCE_BYTES: u64 = 512 * 1024 * 1024;

#[cfg(feature = "import")]
pub use with_package::*;

#[cfg(feature = "import-tiff")]
mod tiff;

#[cfg(feature = "import-pdf")]
mod pdf;

#[cfg(feature = "import-raster")]
mod raster;

#[cfg(any(feature = "import-psd", feature = "import-raw", feature = "import-tiff", feature = "import-raster"))]
mod colour;

#[cfg(feature = "import")]
mod with_package {
    use super::*;
    use hsk_studio_accord::CancellationToken;
    use hsk_studio_folio::{Resolver, Snapshot};
    use hsk_studio_package::{Extras, ImportEntry, Limits as PackageLimits, MemoryAssetSource, WriteOptions, write_package_with};
    use serde_json::Value;
    #[cfg(any(feature = "import-psd", feature = "import-raw"))]
    use hsk_studio_folio as folio;
    #[cfg(any(feature = "import-psd", feature = "import-raw", feature = "import-tiff", feature = "import-raster"))]
    use {hsk_studio_package::ImportRole, serde_json::json};

    /// What an importer produced, ready to be written as a native container.
    pub struct ImportedDocument {
        pub format: SourceFormat,
        pub snapshot: Snapshot,
        /// Digest-keyed tile bytes the document references.
        pub assets: MemoryAssetSource,
        /// Records (when any), receipt, original.
        pub imports: Vec<ImportEntry>,
        /// The receipt as JSON (also stored as the `receipt` import entry).
        pub receipt: Value,
    }

    impl ImportedDocument {
        /// Native container bytes: document, tile assets and the import entries.
        pub fn to_package(&self, cancel: &CancellationToken) -> Result<Vec<u8>, ImportError> {
            let held: Vec<_> = self.imports.iter().map(ImportEntry::to_opaque).collect();
            let mut out = Vec::new();
            write_package_with(
                &self.snapshot,
                &self.assets,
                &Extras { quarantined: &held, previews: &[] },
                &WriteOptions::default(),
                &PackageLimits::default(),
                cancel,
                &mut out,
            )
            .map_err(|e| if e.code() == "canceled" { ImportError::Canceled } else { ImportError::Package(e.code()) })?;
            Ok(out)
        }
    }

    /// Import choices a host makes explicitly (lane SMB-DEVELOP-CAMERA-COLOUR-2). Every field
    /// defaults to the strict behaviour.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct ImportSettings {
        /// Develop a camera raw whose only colour is a single-illuminant approximation (a vendor
        /// file matrix, below STU-COL-218's two calibrations). Default `false`: such a raw refuses
        /// `approximate_colour_not_allowed`. When used, the receipt's `develop.approximate_colour`
        /// is `true` and the loss list names `approximate_colour_single_illuminant`.
        pub approximate_colour: bool,
    }

    /// Sniffs `bytes` and runs the selected importer with the default (strict) settings.
    pub fn import(bytes: &[u8], resolver: &dyn Resolver, cancel: &CancellationToken) -> Result<ImportedDocument, ImportError> {
        import_with(bytes, resolver, cancel, &ImportSettings::default())
    }

    /// [`import`] with explicit [`ImportSettings`].
    pub fn import_with(bytes: &[u8], resolver: &dyn Resolver, cancel: &CancellationToken, settings: &ImportSettings) -> Result<ImportedDocument, ImportError> {
        cancel.check().map_err(|_| ImportError::Canceled)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_SOURCE_BYTES {
            return Err(ImportError::Package("entry_too_large"));
        }
        let format = sniff(bytes).ok_or(ImportError::FormatUnrecognised)?;
        let mut imported = match format {
            SourceFormat::Psd | SourceFormat::Psb => psd_route(bytes, format, resolver, cancel),
            SourceFormat::Tiff => tiff_family(bytes, resolver, cancel, settings),
            SourceFormat::CameraRaw => raw_route(bytes, format, resolver, cancel, settings),
            SourceFormat::Pdf => pdf_route(bytes, resolver, cancel),
            SourceFormat::Png | SourceFormat::Jpeg | SourceFormat::Webp => raster_route(bytes, format, resolver, cancel),
        }?;
        bind_blending_space(&mut imported, resolver, cancel)?;
        Ok(imported)
    }

    /// Lane SMB-UI-SUPPORT-APIS: every importer converts pixels into the document's RGB working
    /// space and composites there, so the converted document states it: an unassigned
    /// `blending_space` is bound to `working_space_rgb` (re-validated by folio) and the receipt
    /// records `blending_space` {bound_to, profile}. A document without an RGB working space
    /// (PDF carried, nothing to composite) keeps it unassigned. Viewers then no longer assume it.
    fn bind_blending_space(imported: &mut ImportedDocument, resolver: &dyn Resolver, cancel: &CancellationToken) -> Result<(), ImportError> {
        use hsk_studio_folio::ProfileBinding;
        let doc = imported.snapshot.document();
        let (ProfileBinding::Unassigned(()), ProfileBinding::Assigned(rgb)) = (&doc.blending_space, &doc.working_space_rgb) else {
            return Ok(());
        };
        let profile = rgb.clone();
        let mut bound = doc.clone();
        bound.blending_space = ProfileBinding::Assigned(profile.clone());
        imported.snapshot = hsk_studio_folio::validate_document(bound, hsk_studio_folio::Budget::default(), cancel, resolver).map_err(|_| ImportError::Package("blending_space_rejected"))?;
        imported.receipt["blending_space"] = serde_json::json!({"bound_to": "working_space_rgb", "profile": profile});
        let receipt = serde_json::to_vec(&imported.receipt).map_err(|_| ImportError::Package("receipt_encode"))?;
        for entry in &mut imported.imports {
            if entry.role == hsk_studio_package::ImportRole::Receipt {
                *entry = ImportEntry::new(hsk_studio_package::ImportRole::Receipt, receipt.clone());
            }
        }
        Ok(())
    }

    /// Import, then native container bytes (convert to native).
    pub fn convert_to_native(
        bytes: &[u8],
        resolver: &dyn Resolver,
        cancel: &CancellationToken,
    ) -> Result<(Vec<u8>, ImportedDocument), ImportError> {
        let imported = import(bytes, resolver, cancel)?;
        let package = imported.to_package(cancel)?;
        Ok((package, imported))
    }

    #[cfg(not(feature = "import-psd"))]
    fn psd_route(_: &[u8], format: SourceFormat, _: &dyn Resolver, _: &CancellationToken) -> Result<ImportedDocument, ImportError> {
        Err(ImportError::FormatUnavailable { format, feature: format.feature(), reason: "feature_not_enabled" })
    }

    /// `%PDF-`: carried, not imported (feature `import-pdf`, module `pdf`).
    #[cfg(feature = "import-pdf")]
    fn pdf_route(bytes: &[u8], resolver: &dyn Resolver, cancel: &CancellationToken) -> Result<ImportedDocument, ImportError> {
        super::pdf::import_pdf(bytes, resolver, cancel)
    }

    #[cfg(not(feature = "import-pdf"))]
    fn pdf_route(_: &[u8], _: &dyn Resolver, _: &CancellationToken) -> Result<ImportedDocument, ImportError> {
        Err(ImportError::FormatUnavailable { format: SourceFormat::Pdf, feature: "import-pdf", reason: "feature_not_enabled" })
    }

    /// PNG / JPEG / WebP through `hsk-studio-interop-raster` (feature `import-raster`, module `raster`).
    #[cfg(feature = "import-raster")]
    fn raster_route(bytes: &[u8], format: SourceFormat, resolver: &dyn Resolver, cancel: &CancellationToken) -> Result<ImportedDocument, ImportError> {
        super::raster::import_raster(bytes, format, resolver, cancel)
    }

    #[cfg(not(feature = "import-raster"))]
    fn raster_route(_: &[u8], format: SourceFormat, _: &dyn Resolver, _: &CancellationToken) -> Result<ImportedDocument, ImportError> {
        Err(ImportError::FormatUnavailable { format, feature: "import-raster", reason: "feature_not_enabled" })
    }

    #[cfg(not(feature = "import-raw"))]
    fn raw_route(_: &[u8], format: SourceFormat, _: &dyn Resolver, _: &CancellationToken, _: &ImportSettings) -> Result<ImportedDocument, ImportError> {
        Err(ImportError::FormatUnavailable { format, feature: "import-raw", reason: "feature_not_enabled" })
    }

    /// TIFF-family bytes: with `import-raw`, `hsk-studio-develop` decides first (DNG, CR2, NEF, ARW;
    /// its typed refusals pass through). A camera raw never falls through to the plain TIFF route
    /// (SMB-CONVERT-FOLLOWUPS slice 2): before it, the interop-tiff container walk (IFD chain and
    /// SubIFDs) looks for DNGVersion (50706) or a CFA (32803) / LinearRaw (34892) photometric
    /// (`tiff::camera_raw_marker`). With `import-raw`, develop's `unsupported_container` continues as
    /// plain TIFF only when no marker is present (otherwise develop's refusal stands); without
    /// `import-raw` a marked file is `format_unavailable` (format `camera_raw`, feature `import-raw`).
    /// Tests: `tests/import_tiff.rs` `camera_raw_never_imports_as_plain_tiff_without_import_raw`.
    fn tiff_family(bytes: &[u8], resolver: &dyn Resolver, cancel: &CancellationToken, settings: &ImportSettings) -> Result<ImportedDocument, ImportError> {
        #[cfg(feature = "import-raw")]
        {
            let not_raw = hsk_studio_develop::DevelopError::UnsupportedContainer.code();
            match raw_route(bytes, SourceFormat::Tiff, resolver, cancel, settings) {
                Err(ImportError::Importer { code, .. }) if code == not_raw && camera_raw_marker(bytes).is_none() => {
                    plain_tiff(bytes, resolver, cancel, "develop_unsupported_container")
                }
                other => other,
            }
        }
        #[cfg(not(feature = "import-raw"))]
        {
            let _ = settings; // only the camera-raw route reads it
            if camera_raw_marker(bytes).is_some() {
                return Err(ImportError::FormatUnavailable {
                    format: SourceFormat::CameraRaw,
                    feature: "import-raw",
                    reason: "feature_not_enabled",
                });
            }
            plain_tiff(bytes, resolver, cancel, "container_markers_absent")
        }
    }

    #[cfg(feature = "import-tiff")]
    fn camera_raw_marker(bytes: &[u8]) -> Option<&'static str> {
        super::tiff::camera_raw_marker(bytes)
    }

    /// Without interop-tiff no container walk is in the build; the plain TIFF route is unavailable too.
    #[cfg(not(feature = "import-tiff"))]
    fn camera_raw_marker(_: &[u8]) -> Option<&'static str> {
        None
    }

    #[cfg(not(feature = "import-tiff"))]
    fn plain_tiff(_: &[u8], _: &dyn Resolver, _: &CancellationToken, _: &'static str) -> Result<ImportedDocument, ImportError> {
        Err(ImportError::FormatUnavailable { format: SourceFormat::Tiff, feature: "import-tiff", reason: "feature_not_enabled" })
    }

    #[cfg(feature = "import-tiff")]
    fn plain_tiff(
        bytes: &[u8],
        resolver: &dyn Resolver,
        cancel: &CancellationToken,
        camera_raw_check: &'static str,
    ) -> Result<ImportedDocument, ImportError> {
        super::tiff::import_tiff(bytes, resolver, cancel, camera_raw_check)
    }

    /// Camera raw through `hsk-studio-develop` (feature `import-raw`): `decode_container` decides the
    /// container (DNG, CR2, NEF; typed refusal otherwise; a TIFF without DNGVersion is plain TIFF, which
    /// has no importer yet), the default crop window is developed with the file's own colour solution
    /// into linear light with sRGB primaries (`color::xyz_d50_to_linear_srgb`; as-shot white balance;
    /// BaselineExposure not applied), and the result goes through interop-psd's `raster_proposal` as
    /// one 32-bit layer, so tiling and digests stay single-owner. Develop refusals pass through unchanged.
    #[cfg(feature = "import-raw")]
    fn raw_route(
        bytes: &[u8],
        format: SourceFormat,
        resolver: &dyn Resolver,
        cancel: &CancellationToken,
        settings: &ImportSettings,
    ) -> Result<ImportedDocument, ImportError> {
        use hsk_studio_develop as develop;
        let prepared = raw_prepare_with(bytes, format, settings, cancel)?;
        let (image, receipt) = develop::develop_to_image(&prepared.plane, &prepared.recipe, 1, cancel).map_err(|e| prepared.refusal(e))?;
        let (window, window_report, colour_route) = (prepared.window, prepared.window_report, prepared.colour_route);
        let (colour_loss, colour_cct, illuminant_weight) = (prepared.colour_loss, prepared.colour_cct, prepared.illuminant_weight);
        let (mut proposal, frames) = raw_proposal(&prepared, image, resolver, cancel)?;
        if let Some(code) = colour_loss {
            proposal.loss.entries.push(hsk_studio_interop_psd::LossEntry { path: "document/colour".into(), kind: hsk_studio_interop_psd::LossKind::Transformed, code });
        }
        drop(prepared);
        let colour = super::colour::summary(
            &proposal,
            frames,
            "develop camera solution -> linear light with sRGB primaries (the working space; no ICC transform); DNG embedded profiles are not read",
        );
        let extra = json!({
            "colour": colour,
            "develop": {
                "route": receipt.route, "profile_route": receipt.profile_route, "math_token": receipt.math_token,
                "demosaic": receipt.demosaic_algorithm,
                "window": [window.x, window.y, window.width, window.height],
                "colour_route": colour_route.key(), "colour_source": format!("{colour_route:?}"), "colour_assumption": colour_route.assumption(),
                "approximate_colour": colour_loss.is_some(), "colour_cct": colour_cct, "illuminant_weight": illuminant_weight,
                "clamped_below": window_report.clamped_below, "clamped_above": window_report.clamped_above,
                "black_rounded": window_report.black_rounded, "black_site_adjusted": window_report.black_site_adjusted,
                "output": "linear light, sRGB primaries (xyz_d50_to_linear_srgb), as-shot white balance, baseline exposure not applied",
            },
        });
        package_proposal(format, "hsk-studio-develop + hsk-studio-interop-psd", bytes, proposal, extra)
    }

    /// A decoded camera raw ready to develop (feature `import-raw`): the default crop window as a CFA
    /// plane and the file's own recipe (colour solution into linear light with sRGB primaries, as-shot
    /// white balance, baseline exposure ignored, every adjustment identity). The import route and the
    /// viewer's develop panel (`view::develop`, lane SMB-HARNESS-DEVELOP-EXPORT) both start here, so a
    /// re-develop with the identity recipe reproduces the imported tiles exactly.
    #[cfg(feature = "import-raw")]
    pub struct RawPrepared {
        pub format: SourceFormat,
        pub window: hsk_studio_develop::PlaneWindow,
        pub window_report: hsk_studio_develop::WindowReport,
        pub plane: hsk_studio_develop::CfaPlane,
        pub recipe: hsk_studio_develop::DevelopRecipe,
        /// The file's DNG colour tags (raw-form white balance re-solves them: `recipe_for_white_point`).
        pub colour: hsk_studio_develop::DngColor,
        /// Where `colour` came from (DNG tags, a vendor matrix tag or the spectral table; lane
        /// SMB-DEVELOP-CAMERA-COLOUR).
        pub colour_route: hsk_studio_develop::ColourRoute,
        /// `Some(approximate_colour_single_illuminant)` when an opted-in approximate route is used.
        pub colour_loss: Option<&'static str>,
        /// Solved scene CCT (kelvin) and the weight toward the higher-temperature calibration
        /// (two-illuminant interpolation, STU-COL-218; 0 for a single calibration).
        pub colour_cct: f64,
        pub illuminant_weight: f64,
        /// XYZ D50 -> linear sRGB primaries, the profile matrix of `recipe`.
        pub to_profile: hsk_studio_develop::color::Mat3,
        source: hsk_studio_interop_psd::SourceIdentity,
        /// The id mint after the working-profile id: every developed raster is tiled from a clone
        /// of it, so document, layer and tile ids are those of the import.
        mint: SourceMint,
    }

    #[cfg(feature = "import-raw")]
    impl RawPrepared {
        /// Develop refusals as importer errors (`Canceled` stays canceled).
        pub fn refusal(&self, e: hsk_studio_develop::DevelopError) -> ImportError {
            match e {
                hsk_studio_develop::DevelopError::Canceled => ImportError::Canceled,
                other => ImportError::Importer { format: self.format, code: other.code() },
            }
        }
    }

    /// Decodes `bytes` with `hsk-studio-develop` and prepares the default window (see [`RawPrepared`]),
    /// strict settings (an approximate colour route refuses).
    #[cfg(feature = "import-raw")]
    pub fn raw_prepare(bytes: &[u8], format: SourceFormat, cancel: &CancellationToken) -> Result<RawPrepared, ImportError> {
        raw_prepare_with(bytes, format, &ImportSettings::default(), cancel)
    }

    /// [`raw_prepare`] with explicit [`ImportSettings`].
    #[cfg(feature = "import-raw")]
    pub fn raw_prepare_with(bytes: &[u8], format: SourceFormat, settings: &ImportSettings, cancel: &CancellationToken) -> Result<RawPrepared, ImportError> {
        use hsk_studio_develop as develop;
        use hsk_studio_interop_psd as psd;
        let refusal = |e: develop::DevelopError| match e {
            develop::DevelopError::Canceled => ImportError::Canceled,
            other => ImportError::Importer { format, code: other.code() },
        };
        let mut raw = develop::decode_container(bytes).map_err(refusal)?;
        if settings.approximate_colour {
            raw.allow_approximate_colour();
        }
        cancel.check().map_err(|_| ImportError::Canceled)?;
        let window = raw.default_window();
        let (plane, window_report) =
            raw.cfa_plane(window, develop::RangePolicy::ClampReported, 1, develop::Limits::LARGE).map_err(refusal)?;
        let source = psd::SourceIdentity::of(bytes);
        let mut mint = SourceMint::new(&source.sha256);
        let profile_id = psd::IdMint::linear_working_profile_id(&mut mint);
        let binding = develop::ProfileBinding {
            profile_id: hsk_studio_accord::DomainId::parse(&profile_id).map_err(|_| ImportError::Package("profile_id"))?,
            sha256: [0; 32],
        };
        let to_srgb = develop::color::xyz_d50_to_linear_srgb().map_err(refusal)?;
        let (recipe, solution) = raw.recipe(&to_srgb, binding, develop::BaselineExposure::Ignore).map_err(refusal)?;
        Ok(RawPrepared {
            format,
            window,
            window_report,
            plane,
            recipe,
            colour_loss: raw.colour_loss(),
            colour_cct: solution.cct,
            illuminant_weight: solution.weight,
            colour: raw.color,
            colour_route: raw.colour_route,
            to_profile: to_srgb,
            source,
            mint,
        })
    }

    /// One developed image as interop-psd's raster proposal (one 32-bit `Background` layer through
    /// `raster_proposal` and the Prism converter, the import's tiling) and the Prism frame count.
    #[cfg(feature = "import-raw")]
    pub fn raw_proposal(
        prepared: &RawPrepared,
        image: hsk_studio_develop::DevelopedImage,
        resolver: &dyn Resolver,
        cancel: &CancellationToken,
    ) -> Result<(hsk_studio_interop_psd::ImportProposal, u64), ImportError> {
        use hsk_studio_interop_psd as psd;
        let format = prepared.format;
        let mut mint = prepared.mint.clone();
        let mut frames = super::colour::FrameCount::default();
        let mut converter = super::colour::converter(format, &mint, &mut frames)?;
        let pixels = (image.width as usize).checked_mul(image.height as usize).ok_or(ImportError::Package("dimensions"))?;
        if Some(image.rgb.len()) != pixels.checked_mul(3) {
            return Err(ImportError::Package("developed_image_size"));
        }
        let mut planes: Vec<Vec<u8>> = (0..3).map(|_| Vec::with_capacity(pixels.saturating_mul(4))).collect();
        for px in image.rgb.chunks_exact(3) {
            for (plane, v) in planes.iter_mut().zip(px) {
                plane.extend_from_slice(&v.to_be_bytes());
            }
        }
        let (width, height) = (image.width, image.height);
        drop(image);
        let raster = psd::RasterSource {
            format: if format == SourceFormat::Tiff { "dng" } else { "camera_raw" },
            name: "Background",
            width,
            height,
            depth: 32,
            colour_mode: psd::ColorMode::Rgb,
            planes: planes.into_iter().map(|bytes| psd::Plane { width, height, depth: 32, bytes }).collect(),
            alpha: None,
            icc: None,
        };
        let limits = psd::Limits::default();
        let cfg = psd::ProposalConfig::new(&limits, folio::Budget::default(), resolver);
        let psd_refusal = |e: psd::PsdError| {
            if e == psd::PsdError::Canceled { ImportError::Canceled } else { ImportError::Importer { format, code: e.code() } }
        };
        let proposal =
            psd::raster_proposal(raster, &prepared.source, &cfg, &mut mint, Some(&mut converter), cancel).map_err(psd_refusal)?;
        drop(converter);
        Ok((proposal, frames.0))
    }

    #[cfg(feature = "import-psd")]
    fn psd_route(
        bytes: &[u8],
        format: SourceFormat,
        resolver: &dyn Resolver,
        cancel: &CancellationToken,
    ) -> Result<ImportedDocument, ImportError> {
        use hsk_studio_interop_psd as psd;
        let refusal = |e: psd::PsdError| {
            if e == psd::PsdError::Canceled { ImportError::Canceled } else { ImportError::Importer { format, code: e.code() } }
        };
        let limits = psd::Limits::default();
        let doc = psd::read_psd(bytes, &limits, cancel).map_err(refusal)?;
        let source = psd::SourceIdentity::of(bytes);
        let cfg = psd::ProposalConfig::new(&limits, folio::Budget::default(), resolver);
        let mut mint = SourceMint::new(&source.sha256);
        let mut frames = super::colour::FrameCount::default();
        let mut converter = super::colour::converter(format, &mint, &mut frames)?;
        let proposal = psd::to_proposal(&doc, &source, &cfg, &mut mint, Some(&mut converter), cancel).map_err(refusal)?;
        drop(converter);
        let colour = super::colour::summary(&proposal, frames.0, "embedded PSD profile (resource 1039) -> working space");

        let p = &proposal.provenance;
        let extra = json!({
            "colour": colour,
            "provenance": {
                "psd_version": p.psd_version, "width": p.width, "height": p.height, "depth": p.depth,
                "assumed_profile": p.assumed_profile, "icc_converted": p.icc_converted, "icc_sha256": p.icc_sha256_hex,
                "sixteen_bit_scale": format!("{:?}", p.sixteen_bit_scale), "display_conversion": p.display.is_some(),
            },
            "layer_map": proposal.layer_map.iter().map(|m| json!({"layer_id":m.layer_id,"psd_index":m.psd_index})).collect::<Vec<_>>(),
            "layer_extras": proposal.extras.iter().map(|x| json!({
                "layer_id": x.layer_id, "psd_index": x.psd_index, "blend_key": psd::key_text(x.blend_key),
                "group_blend_key": x.group_blend_key.map(psd::key_text), "blend_clipped_elements": x.blend_clipped_elements,
                "blend_interior_elements": x.blend_interior_elements, "knockout": x.knockout, "mask_default_color": x.mask_default_color,
            })).collect::<Vec<_>>(),
        });
        package_proposal(format, "hsk-studio-interop-psd", bytes, proposal, extra)
    }

    /// Shared tail of every importer that yields an interop-psd `ImportProposal` (PSD layers, or one
    /// raster through `raster_proposal`): tile assets, records, receipt, original.
    #[cfg(any(feature = "import-psd", feature = "import-raw", feature = "import-tiff", feature = "import-raster"))]
    pub(crate) fn package_proposal(
        format: SourceFormat,
        importer: &str,
        bytes: &[u8],
        proposal: hsk_studio_interop_psd::ImportProposal,
        extra: Value,
    ) -> Result<ImportedDocument, ImportError> {
        let mut assets = MemoryAssetSource::new();
        for tile in &proposal.tiles {
            let mut both = Vec::with_capacity(tile.colour.len().saturating_add(tile.alpha.len()));
            both.extend_from_slice(&tile.colour);
            both.extend_from_slice(&tile.alpha);
            assets.insert(both);
        }
        for mask in &proposal.mask_tiles {
            assets.insert(mask.coverage.clone());
        }
        let mut imports = Vec::new();
        if !proposal.raster_records.layers.is_empty() {
            let records = serde_json::to_vec(&proposal.raster_records).map_err(|_| ImportError::Package("records_encode"))?;
            imports.push(ImportEntry::new(ImportRole::Records, records));
        }
        let original = ImportEntry::new(ImportRole::Original, bytes.to_vec());
        let p = &proposal.provenance;
        let mut receipt = json!({
            "receipt": "import",
            "receipt_version": 1,
            "source_format": p.source_format,
            "importer": importer,
            "source": {"sha256": p.source_sha256_hex, "byte_len": p.byte_len, "entry": original.name()},
            "layers": proposal.snapshot.document().layers.len(),
            "tiles": proposal.tiles.len(),
            "mask_tiles": proposal.mask_tiles.len(),
            "records": !proposal.raster_records.layers.is_empty(),
            "loss": proposal.loss.entries.iter().map(|e| json!({"path":e.path,"kind":e.kind.as_str(),"code":e.code})).collect::<Vec<_>>(),
            "not_proven": RECEIPT_NOT_PROVEN,
        });
        if let (Some(target), Some(fields)) = (receipt.as_object_mut(), extra.as_object()) {
            for (k, v) in fields {
                target.insert(k.clone(), v.clone());
            }
        }
        let receipt_bytes = serde_json::to_vec(&receipt).map_err(|_| ImportError::Package("receipt_encode"))?;
        imports.push(ImportEntry::new(ImportRole::Receipt, receipt_bytes));
        imports.push(original);
        Ok(ImportedDocument { format, snapshot: proposal.snapshot, assets, imports, receipt })
    }

    /// What every receipt of this slice states it does not prove.
    pub const RECEIPT_NOT_PROVEN: &[&str] = &[
        "colour_oracle_parity (embedded profiles convert through Prism into linear sRGB; no Photoshop/LittleCMS oracle comparison of the converted pixels in this flow)",
        "host_identity_minting (SourceMint is a deterministic standalone stand-in seeded by the source digest)",
        "photoshop_parity (no Photoshop-authored oracle comparison in this flow)",
    ];

    /// Deterministic standalone identity minting for converted documents: UUIDv7-shaped ids whose
    /// first 48 bits are the source digest's and whose tail is a counter. The same source always
    /// converts to the same ids (byte-identical packages); it is a stand-in for a host `IdMint`.
    #[cfg(any(feature = "import-psd", feature = "import-raw", feature = "import-tiff", feature = "import-raster", feature = "import-pdf"))]
    #[derive(Clone)]
    pub struct SourceMint {
        seed: String,
        n: u64,
    }

    #[cfg(any(feature = "import-psd", feature = "import-raw", feature = "import-tiff", feature = "import-raster", feature = "import-pdf"))]
    impl SourceMint {
        pub fn new(sha256: &[u8; 32]) -> Self {
            let hex: String = sha256.iter().take(6).map(|b| format!("{b:02x}")).collect();
            let (time_high, time_low) = hex.split_at(8);
            Self { seed: format!("{time_high}-{time_low}"), n: 0 }
        }

        /// Ids outside the counter range (`ffff` + `k`), for objects that are not document
        /// identities (colour profile and conversion resource ids).
        #[cfg(any(feature = "import-psd", feature = "import-raw", feature = "import-tiff", feature = "import-raster"))]
        pub(crate) fn reserved(&self, prefix: &str, k: u32) -> String {
            format!("{prefix}-{}-7000-8000-ffff{k:08x}", self.seed)
        }

        /// Same seed as [`SourceMint::new`] from the lowercase hex SHA-256 of the source.
        #[cfg(feature = "import-pdf")]
        pub(crate) fn from_sha256_hex(hex: &str) -> Self {
            let head = hex.get(..12).unwrap_or("000000000000");
            let (time_high, time_low) = head.split_at(8);
            Self { seed: format!("{time_high}-{time_low}"), n: 0 }
        }

        pub(crate) fn next(&mut self, prefix: &str) -> String {
            self.n = self.n.saturating_add(1);
            format!("{prefix}-{}-7000-8000-{:012x}", self.seed, self.n)
        }
    }

    #[cfg(any(feature = "import-psd", feature = "import-raw", feature = "import-tiff", feature = "import-raster"))]
    impl hsk_studio_interop_psd::IdMint for SourceMint {
        fn document_id(&mut self) -> String {
            self.next("SDOC")
        }
        fn artboard_id(&mut self) -> String {
            self.next("SART")
        }
        fn layer_id(&mut self) -> String {
            self.next("SLYR")
        }
        fn linear_working_profile_id(&mut self) -> String {
            self.next("SCPF")
        }
        fn tile_manifest_id(&mut self, layer_id: &str, object_key: &str) -> String {
            format!("package/{layer_id}/{object_key}")
        }
    }
}
