use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CkcSourceRefError<E> {
    Validation(String),
    RuntimeRef(E),
}

pub const CKC_FOLDER_REF_KIND: &str = "folder";
pub const CKC_SOURCE_URL_REF_KIND: &str = "source_url";

/// The two CKC source-provenance ref kinds a media album membership or a media
/// asset can carry (WP-CKC-posekit-overhaul MT-035). `Folder` is a portable
/// folder/source-path ref; `SourceUrl` is an http(s)/source-url:// ref.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CkcSourceRefKind {
    Folder,
    SourceUrl,
}

impl CkcSourceRefKind {
    pub fn ref_kind(self) -> &'static str {
        match self {
            CkcSourceRefKind::Folder => CKC_FOLDER_REF_KIND,
            CkcSourceRefKind::SourceUrl => CKC_SOURCE_URL_REF_KIND,
        }
    }
}

/// A validated source ref together with its stable `ref_kind` token, the
/// shape the CKC media surfaces return to the frontend.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CkcSourceRefReadout {
    pub ref_kind: &'static str,
    pub value: String,
}

pub fn ckc_source_ref_readout<E>(
    kind: CkcSourceRefKind,
    value: &str,
    reject_runtime_ref: &mut impl FnMut(&str, &str) -> Result<(), E>,
) -> Result<CkcSourceRefReadout, CkcSourceRefError<E>> {
    validate_ckc_source_ref(kind, "source_ref", value, reject_runtime_ref)?;
    Ok(CkcSourceRefReadout {
        ref_kind: kind.ref_kind(),
        value: value.to_owned(),
    })
}

pub fn optional_ckc_source_ref_readout<E>(
    kind: CkcSourceRefKind,
    value: Option<&str>,
    reject_runtime_ref: &mut impl FnMut(&str, &str) -> Result<(), E>,
) -> Result<Option<CkcSourceRefReadout>, CkcSourceRefError<E>> {
    value
        .map(|value| ckc_source_ref_readout(kind, value, reject_runtime_ref))
        .transpose()
}

pub fn normalize_optional_ckc_source_ref<E>(
    kind: CkcSourceRefKind,
    field: &str,
    value: &Option<String>,
    reject_runtime_ref: &mut impl FnMut(&str, &str) -> Result<(), E>,
) -> Result<Option<String>, CkcSourceRefError<E>> {
    match value.as_deref() {
        None => Ok(None),
        Some(raw) => {
            validate_ckc_source_ref(kind, field, raw, reject_runtime_ref)?;
            Ok(Some(raw.to_owned()))
        }
    }
}

/// Read-side redaction: a stored ref that no longer passes
/// [`validate_ckc_source_ref`] (schema drift, a row written before the rule
/// existed) is dropped from API responses instead of being leaked. Callers
/// surface the drop as a `redacted_invalid` status.
pub fn portable_optional_ckc_source_ref<E>(
    kind: CkcSourceRefKind,
    field: &str,
    value: Option<String>,
    reject_runtime_ref: &mut impl FnMut(&str, &str) -> Result<(), E>,
) -> Option<String> {
    value.filter(|raw| validate_ckc_source_ref(kind, field, raw, reject_runtime_ref).is_ok())
}

pub fn validate_ckc_source_ref<E>(
    kind: CkcSourceRefKind,
    field: &str,
    value: &str,
    reject_runtime_ref: &mut impl FnMut(&str, &str) -> Result<(), E>,
) -> Result<(), CkcSourceRefError<E>> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed != value {
        return Err(CkcSourceRefError::Validation(format!(
            "{field} must not be empty or padded"
        )));
    }
    reject_runtime_ref(field, value).map_err(CkcSourceRefError::RuntimeRef)?;
    let lower = value.to_ascii_lowercase();
    if lower.contains(":///") {
        return Err(CkcSourceRefError::Validation(format!(
            "{field} must include a portable ref authority before its path"
        )));
    }
    match kind {
        CkcSourceRefKind::Folder => {
            if lower.starts_with("http://")
                || lower.starts_with("https://")
                || lower.starts_with("source-url://")
            {
                return Err(CkcSourceRefError::Validation(format!(
                    "{field} must be a folder/source-path ref, not a source URL ref"
                )));
            }
            if lower.starts_with("atelier://folder/")
                || lower.starts_with("source://")
                || lower.starts_with("dataset://")
                || lower.starts_with("artifact://")
                || lower.starts_with("manifest://")
            {
                return Ok(());
            }
            Err(CkcSourceRefError::Validation(format!(
                "{field} must use atelier://folder/, source://, dataset://, artifact://, or manifest://"
            )))
        }
        CkcSourceRefKind::SourceUrl => {
            if lower.starts_with("http://")
                || lower.starts_with("https://")
                || lower.starts_with("source-url://")
            {
                return Ok(());
            }
            Err(CkcSourceRefError::Validation(format!(
                "{field} must be an http(s) or source-url:// ref"
            )))
        }
    }
}
