//! Studio settings catalog (lane SMB-SETTINGS-SCHEMA-1, RF-09 phase 0, asset side of MT-36249
//! AC-HOST-001; GUI-DESKTOP-014/015, SET-REC-001..004). No toolkit code.
//!
//! # Manual (no-context operator or model)
//!
//! - **Catalog.** [`studio_settings_catalog`] joins [`CATALOG_GROUPS`] into one accord
//!   [`SettingsSchema`]: the brush entries ([`super::edit::BRUSH_SETTING_ENTRIES`], module tab
//!   `harness-view`, subtab `brush`), the selection-tool entries
//!   (`hsk_studio_pigment::settings::SELECTION_SETTING_ENTRIES`, `pigment` / `selection`) and the
//!   export-stage entries (`hsk_studio_develop::settings::EXPORT_SETTING_ENTRIES`, `develop` /
//!   `export`). Every refusal of the schema build is typed ([`SchemaRefusal`]); a host projects the
//!   schema with `SettingsSchema::to_preference_json` and validates a value with
//!   `SettingsSchema::validate` before its PreferenceRecord write.
//! - **Route.** [`find_route`] walks the tree (module tab, then feature subtab, then entry) from
//!   the id's own segments: the model-navigation route of an entry (GUI-DESKTOP-015). An id outside
//!   the grammar or the tree has no route.
//! - **Audit.** [`audit_catalog`] builds the catalog and walks every entry to its route; the
//!   [`CatalogAudit`] carries counters and a typed code only. [`report_catalog_audit`] sends it as
//!   one Observe terminal frame (codes and counters, no labels or text): success with
//!   `[entries, found, found, found, 0]` (read-only, nothing changed), or a validation failure
//!   (`invalid_input` for a schema refusal, `validation_rejected` for an entry without a route)
//!   with `[entries, found, 0, 0, 0]`. Actor, resource and correlation come from the caller.
//! - **Scope.** Phase 0: typed entries and the tree only. The Settings tab, persistence through
//!   PreferenceRecord, Handpick search and controls Registry ids are MT-36249 phase 1. The namespace
//!   `studio` is not a SET-SCOPE-001 namespace (`namespace_status` `spec_delta_pending`, SD-07).
use hsk_studio_accord::{
    ActorContext, CancellationToken, DomainId, SchemaRefusal, SettingEntry, SettingId, SettingsError, SettingsModule,
    SettingsSchema,
};
use hsk_studio_observe::{
    Budget, DiagnosticCode, DiagnosticDetail, Disposition, Error as ObserveError, FailureCode, Observation, Observe, Outcome,
    Receipt, SinkPort,
};

/// The entry groups of the Studio catalog, in tab order.
pub const CATALOG_GROUPS: [&[SettingEntry]; 3] = [
    super::edit::BRUSH_SETTING_ENTRIES,
    hsk_studio_pigment::settings::SELECTION_SETTING_ENTRIES,
    hsk_studio_develop::settings::EXPORT_SETTING_ENTRIES,
];

/// The Studio catalog: one schema over every group.
pub fn studio_settings_catalog() -> Result<SettingsSchema, SchemaRefusal> {
    SettingsSchema::new(&CATALOG_GROUPS)
}

/// Where an entry sits in the Settings hierarchy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingsRoute {
    pub module: &'static str,
    pub feature: &'static str,
    pub id: &'static str,
}

/// Walks `tree` to `id`: module tab, then feature subtab, then entry.
pub fn find_route(tree: &[SettingsModule], id: &str) -> Option<SettingsRoute> {
    let parsed = SettingId::parse(id).ok()?;
    let module = tree.iter().find(|m| m.module == parsed.module())?;
    let feature = module.features.iter().find(|f| f.feature == parsed.feature())?;
    let id = feature.entries.iter().copied().find(|e| *e == id)?;
    Some(SettingsRoute { module: module.module, feature: feature.feature, id })
}

/// The audit's counters and its typed code (`None` when every entry has its route).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CatalogAudit {
    pub modules: u32,
    pub features: u32,
    pub entries: u32,
    pub found: u32,
    /// The schema build refused (`code` is the refusal's); otherwise `code` names a missing route.
    pub schema_refused: bool,
    pub code: Option<&'static str>,
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Audits `groups` as a catalog (the Studio catalog is [`audit_catalog`]).
pub fn audit_groups(groups: &[&[SettingEntry]]) -> CatalogAudit {
    let declared = count(groups.iter().map(|g| g.len()).fold(0usize, usize::saturating_add));
    let schema = match SettingsSchema::new(groups) {
        Ok(s) => s,
        Err(refusal) => {
            return CatalogAudit {
                modules: 0,
                features: 0,
                entries: declared,
                found: 0,
                schema_refused: true,
                code: Some(refusal.error.code()),
            };
        }
    };
    let tree = schema.tree();
    let features = tree.iter().map(|m| m.features.len()).fold(0usize, usize::saturating_add);
    let found = schema
        .entries()
        .iter()
        .filter(|e| find_route(&tree, e.id).is_some_and(|r| r.module == e.module && r.feature == e.feature))
        .count();
    let entries = schema.entries().len();
    CatalogAudit {
        modules: count(tree.len()),
        features: count(features),
        entries: count(entries),
        found: count(found),
        schema_refused: false,
        code: (found != entries).then_some(SettingsError::UnknownSetting.code()),
    }
}

/// Audits the Studio catalog.
pub fn audit_catalog() -> CatalogAudit {
    audit_groups(&CATALOG_GROUPS)
}

/// Sends `audit` as one Observe terminal frame (codes and counters only; see the module notes).
pub fn report_catalog_audit(
    audit: &CatalogAudit,
    correlation_id: u64,
    revision: u64,
    resource: DomainId,
    actor: ActorContext,
    sink: &mut (impl SinkPort + ?Sized),
) -> Result<Receipt, ObserveError> {
    let mut observe = Observe::new(correlation_id, revision, resource, actor, Budget::new(0, 0)?);
    let (outcome, detail) = match audit.code {
        None => (
            Outcome::Success,
            DiagnosticDetail::new(Disposition::NoChange, None, None, [audit.entries, audit.found, audit.found, audit.found, 0])?,
        ),
        Some(_) => (
            Outcome::Failure(FailureCode::Validation),
            DiagnosticDetail::new(
                Disposition::Rejected,
                Some(if audit.schema_refused { DiagnosticCode::InvalidInput } else { DiagnosticCode::ValidationRejected }),
                None,
                [audit.entries, audit.found, 0, 0, 0],
            )?,
        ),
    };
    let event = Observation { correlation_id, revision, outcome, progress: None, private_project_text: None };
    // A fresh token: the audit decided its outcome; a later cancel must not recast it.
    observe.emit_detail(event, &detail, &CancellationToken::default(), sink)
}
