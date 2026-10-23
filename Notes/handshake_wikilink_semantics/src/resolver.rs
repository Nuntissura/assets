//! Wikilink resolution by exact ref/id, normalized title, or in-memory alias.
//!
//! Resolution checks exact ref/id before title, then alias; ambiguity remains explicit.
//!
//! ## Obsidian-default normalization (impl note 2 + the MT contract)
//!
//! A target is normalized for COMPARISON by [`normalize_target`]: trim, collapse internal whitespace
//! runs to a single space, and lower-case (Obsidian's default is case-insensitive title matching).
//! The ORIGINAL-case display title is kept on the index entry for rendering, so a resolved link still
//! shows the canonical title.
//!
//! ## Aliases-backend gap (the typed-blocker path — AC-006 / RISK-002 / MC-002)
//!
//! The backend Loom search / knowledge-document payload does NOT currently expose an `aliases` field
//! (grep-confirmed against `backend/loom.rs` + `backend/knowledge_documents.rs`). So a
//! [`ResolverIndex`] built from the real payload has `aliases_supported = false` and an EMPTY
//! `by_alias` map UNLESS the in-session local stub (the MT-017 PropertiesPanel) populates it. The
//! alias code path is still fully exercised + testable via the local stub; the editor renders a
//! VISIBLE local-only banner so the operator is never misled into thinking aliases persist (the
//! banner + the typed blocker are owned by the runtime / widget — this module is the pure resolution
//! core). The alias stub is IN-MEMORY only: NO SQLite, NO file (AC-007 / MC-006).

use std::collections::HashMap;

/// How a wikilink target was matched to a document. Carried on a [`WikilinkResolution::Resolved`] so
/// the click handler + a test can prove WHICH resolution rule fired (the deterministic-order proof,
/// RISK-004 / MC-004). The enum is matched exhaustively — there is no wildcard arm that could swallow
/// a new match kind silently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchKind {
    /// Matched by an exact backend ref/id (the strongest, first-checked rule).
    ExactRef,
    /// Matched by an exact title (case-insensitive, whitespace-normalized — Obsidian default).
    ExactTitle,
    /// Matched by a declared alias (the weakest rule; only reached when ref + title both missed).
    /// Carries the alias that matched so the UI/AccessKit can surface WHICH alias resolved.
    Alias {
        /// The (original-case) alias text that matched.
        alias: String,
    },
}

/// The result of resolving a `[[Title]]` target against a [`ResolverIndex`]. `Resolved` carries the
/// target document id + the [`MatchKind`] that fired; `Unresolved` carries the (trimmed, original-case)
/// title the create-from-unresolved affordance offers to create. `Ambiguous` is a first-class state:
/// normalized duplicate titles must never silently select whichever document was indexed last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WikilinkResolution {
    /// The target resolved to a live document.
    Resolved {
        /// The resolved document id (the navigation target / the value the mark rewrites to).
        document_id: String,
        /// Which rule matched (ref / title / alias).
        matched_by: MatchKind,
    },
    /// Multiple live documents share the same normalized title. Navigation and create-note actions
    /// are both disabled until the operator chooses an exact document identity.
    Ambiguous {
        /// The trimmed title that was resolved.
        title: String,
        /// Stable, sorted candidate document ids.
        document_ids: Vec<String>,
    },
    /// No document matched the target — the click offers a "Create note \"{title}\"" affordance.
    Unresolved {
        /// The (trimmed, original-case) title to create / display.
        title: String,
    },
}

impl WikilinkResolution {
    /// True when the target resolved to a document.
    pub fn is_resolved(&self) -> bool {
        matches!(self, WikilinkResolution::Resolved { .. })
    }

    /// The resolved document id, if resolved.
    pub fn document_id(&self) -> Option<&str> {
        match self {
            WikilinkResolution::Resolved { document_id, .. } => Some(document_id.as_str()),
            WikilinkResolution::Ambiguous { .. } | WikilinkResolution::Unresolved { .. } => None,
        }
    }
}

/// One document the resolver can match against: its id, its original-case display title, and any
/// declared aliases (original-case). Built from a Loom search hit (title) + the alias stub (aliases).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolverDocument {
    /// The backend document/block id.
    pub document_id: String,
    /// The original-case title (kept for rendering; comparison uses the normalized form).
    pub display_title: String,
    /// The declared aliases (original-case). Empty unless the alias stub populated them
    /// (backend aliases are unavailable — AC-006).
    pub aliases: Vec<String>,
}

/// The resolution index: a normalized-title -> document_id map, a normalized-alias -> document_id map,
/// and per-document display titles, plus the `aliases_supported` flag (false while the backend payload
/// lacks an `aliases` field — AC-006 / RISK-002).
///
/// Built incrementally via [`Self::add_document`] from the MT-038 Loom search enumeration (titles) +
/// the in-session alias stub. A title-vs-alias COLLISION is resolved by [`resolve_wikilink`]'s order
/// (exact title wins), so both maps may legitimately hold the same normalized key pointing at
/// different documents.
#[derive(Debug, Clone, Default)]
pub struct ResolverIndex {
    /// normalized-title -> all matching document ids. A vector with more than one member is an
    /// explicit ambiguity, never a last-write-wins resolution.
    by_title: HashMap<String, Vec<String>>,
    /// normalized-alias -> all matching document ids. Alias collisions are explicit ambiguity,
    /// never insertion-order-dependent last-write-wins resolution.
    by_alias: HashMap<String, Vec<String>>,
    /// document_id -> the document's display metadata (title + aliases), for candidate rendering.
    documents: HashMap<String, ResolverDocument>,
    /// Whether the backend payload exposes an `aliases` field. `false` (the current backend reality)
    /// means `by_alias` is populated ONLY by the in-session local stub, and the editor must render a
    /// local-only banner (AC-006 / MC-002). `true` would mean the backend supplied aliases.
    pub aliases_supported: bool,
}

impl ResolverIndex {
    /// A fresh empty index. `aliases_supported` starts `false` (the current backend reality — the
    /// payload has no `aliases` field, grep-confirmed). The builder sets it `true` only if a real
    /// backend alias source is wired (no such source exists today, so it stays `false` and the
    /// local-only banner shows).
    pub fn new() -> Self {
        Self {
            by_title: HashMap::new(),
            by_alias: HashMap::new(),
            documents: HashMap::new(),
            aliases_supported: false,
        }
    }

    /// Add a document to the index from its id + display title (the MT-038 Loom search enumeration
    /// path). Inserts into `by_title` under the normalized title and records the display metadata. A
    /// blank/whitespace-only title is recorded for rendering but NOT inserted into `by_title` (an
    /// empty normalized key must never resolve every empty `[[]]`).
    pub fn add_document(
        &mut self,
        document_id: impl Into<String>,
        display_title: impl Into<String>,
    ) {
        let document_id = document_id.into();
        let display_title = display_title.into();
        let norm = normalize_target(&display_title);
        if let Some(previous) = self.documents.get(&document_id) {
            let previous_norm = normalize_target(&previous.display_title);
            if previous_norm != norm {
                let remove_key = self
                    .by_title
                    .get_mut(&previous_norm)
                    .map(|ids| {
                        ids.retain(|id| id != &document_id);
                        ids.is_empty()
                    })
                    .unwrap_or(false);
                if remove_key {
                    self.by_title.remove(&previous_norm);
                }
            }
        }
        if !norm.is_empty() {
            let ids = self.by_title.entry(norm).or_default();
            if !ids.iter().any(|id| id == &document_id) {
                ids.push(document_id.clone());
                ids.sort();
            }
        }
        let entry = self
            .documents
            .entry(document_id.clone())
            .or_insert_with(|| ResolverDocument {
                document_id: document_id.clone(),
                display_title: display_title.clone(),
                aliases: Vec::new(),
            });
        entry.display_title = display_title;
    }

    /// Declare an alias for an already-known (or new) document — the in-session LOCAL alias stub path
    /// (the MT-017 PropertiesPanel sets these in-process while the backend lacks the field). Inserts
    /// into `by_alias` under the normalized alias and records the original-case alias on the document
    /// for candidate rendering. A blank alias is ignored. This NEVER writes a file or DB (AC-007 /
    /// MC-006) — it mutates only the in-memory maps.
    pub fn add_alias(&mut self, document_id: impl Into<String>, alias: impl Into<String>) {
        let document_id = document_id.into();
        let alias = alias.into();
        let norm = normalize_target(&alias);
        if norm.is_empty() {
            return;
        }
        let ids = self.by_alias.entry(norm).or_default();
        if !ids.iter().any(|id| id == &document_id) {
            ids.push(document_id.clone());
            ids.sort();
        }
        let doc = self
            .documents
            .entry(document_id.clone())
            .or_insert_with(|| ResolverDocument {
                document_id: document_id.clone(),
                display_title: document_id.clone(),
                aliases: Vec::new(),
            });
        if !doc.aliases.iter().any(|a| a.eq_ignore_ascii_case(&alias)) {
            doc.aliases.push(alias);
        }
    }

    /// The number of documents enumerated (titles indexed).
    pub fn title_count(&self) -> usize {
        self.by_title.len()
    }

    /// The number of alias entries (always 0 unless the local stub populated them — backend aliases
    /// are unavailable, AC-006).
    pub fn alias_count(&self) -> usize {
        self.by_alias.len()
    }

    /// Every indexed document's metadata (id + display title + aliases), for the alias-aware
    /// candidate provider (native `autocomplete::candidates_for_query`).
    pub fn documents(&self) -> impl Iterator<Item = &ResolverDocument> {
        self.documents.values()
    }

    /// The display title for a resolved document id, if known (for the mark-rewrite label).
    pub fn display_title(&self, document_id: &str) -> Option<&str> {
        self.documents
            .get(document_id)
            .map(|d| d.display_title.as_str())
    }
}

/// Normalize a wikilink target for COMPARISON, matching Obsidian's default: trim leading/trailing
/// whitespace, collapse internal whitespace runs to a single space, and lower-case. The original-case
/// form is preserved separately for display (this is comparison-only). Unicode lower-casing is used
/// (so a CJK / accented title normalizes consistently with the rest of the editor's char discipline).
pub fn normalize_target(target: &str) -> String {
    target
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Resolve a wikilink `target` against `index` with the DETERMINISTIC order (RISK-004 / MC-004):
///
/// 1. exact ref/id — the raw target equals a known `document_id` (no normalization; an id is opaque).
/// 2. exact title — the normalized target matches a `by_title` entry (Obsidian-default
///    case-insensitive, whitespace-normalized).
/// 3. alias — the normalized target matches a `by_alias` entry.
///
/// First hit wins; a title that ALSO exists as an alias on another document resolves to the EXACT
/// TITLE (step 2 is checked before step 3), so a title/alias collision is deterministic and
/// title-favoring. No match yields [`WikilinkResolution::Unresolved`] carrying the trimmed
/// original-case title (the create-from-unresolved affordance uses it).
pub fn resolve_wikilink(index: &ResolverIndex, target: &str) -> WikilinkResolution {
    // 1) Exact ref/id: an id is opaque, compared verbatim (only trimmed).
    let trimmed = target.trim();
    if !trimmed.is_empty() && index.documents.contains_key(trimmed) {
        return WikilinkResolution::Resolved {
            document_id: trimmed.to_owned(),
            matched_by: MatchKind::ExactRef,
        };
    }

    let norm = normalize_target(target);
    if norm.is_empty() {
        return WikilinkResolution::Unresolved {
            title: trimmed.to_owned(),
        };
    }

    // 2) Exact title (normalized) — checked BEFORE alias so a title/alias collision favors the title.
    if let Some(document_ids) = index.by_title.get(&norm) {
        if document_ids.len() == 1 {
            return WikilinkResolution::Resolved {
                document_id: document_ids[0].clone(),
                matched_by: MatchKind::ExactTitle,
            };
        }
        return WikilinkResolution::Ambiguous {
            title: trimmed.to_owned(),
            document_ids: document_ids.clone(),
        };
    }

    // 3) Alias (normalized).
    if let Some(document_ids) = index.by_alias.get(&norm) {
        if document_ids.len() != 1 {
            return WikilinkResolution::Ambiguous {
                title: trimmed.to_owned(),
                document_ids: document_ids.clone(),
            };
        }
        let document_id = &document_ids[0];
        // Surface the ORIGINAL-case alias text that matched (for the MatchKind::Alias label).
        let alias = index
            .documents
            .get(document_id)
            .and_then(|d| {
                d.aliases
                    .iter()
                    .find(|a| normalize_target(a) == norm)
                    .cloned()
            })
            .unwrap_or_else(|| trimmed.to_owned());
        return WikilinkResolution::Resolved {
            document_id: document_id.clone(),
            matched_by: MatchKind::Alias { alias },
        };
    }

    // No match -> unresolved (the create affordance uses the trimmed original-case title).
    WikilinkResolution::Unresolved {
        title: trimmed.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> ResolverIndex {
        let mut idx = ResolverIndex::new();
        idx.add_document("DOC-1", "Project Atlas");
        idx.add_document("DOC-2", "Meeting   Notes"); // internal double space -> normalizes to single
        idx.add_document("DOC-3", "Roadmap");
        idx
    }

    #[test]
    fn resolves_exact_title_case_insensitive_and_whitespace_normalized() {
        let idx = index();
        // Exact title, different case + extra/odd whitespace -> ExactTitle match.
        let r = resolve_wikilink(&idx, "  project   ATLAS  ");
        assert_eq!(
            r,
            WikilinkResolution::Resolved {
                document_id: "DOC-1".into(),
                matched_by: MatchKind::ExactTitle
            },
            "Obsidian-default case-insensitive + whitespace-normalized title match"
        );
        // The internal-double-space title still resolves from a single-space query.
        assert_eq!(
            resolve_wikilink(&idx, "meeting notes").document_id(),
            Some("DOC-2")
        );
    }

    #[test]
    fn resolves_by_exact_ref_id_before_title() {
        let idx = index();
        // The raw id is matched verbatim (ExactRef) — strongest rule.
        let r = resolve_wikilink(&idx, "DOC-3");
        assert_eq!(
            r,
            WikilinkResolution::Resolved {
                document_id: "DOC-3".into(),
                matched_by: MatchKind::ExactRef
            }
        );
    }

    #[test]
    fn resolves_by_alias_returns_match_kind_alias() {
        // AC-003: an alias-only target resolves with MatchKind::Alias (the local stub seeds it because
        // the backend has no aliases field — AC-006).
        let mut idx = index();
        idx.add_alias("DOC-1", "Atlas");
        let r = resolve_wikilink(&idx, "atlas");
        assert_eq!(
            r,
            WikilinkResolution::Resolved {
                document_id: "DOC-1".into(),
                matched_by: MatchKind::Alias {
                    alias: "Atlas".into()
                }
            },
            "AC-003: an alias-only target resolves by alias and reports MatchKind::Alias"
        );
    }

    #[test]
    fn title_wins_over_colliding_alias_mc004() {
        // RISK-004 / MC-004: an alias on DOC-A equals a TITLE on DOC-B. Resolution is deterministic:
        // the EXACT TITLE wins (step 2 before step 3), never the alias.
        let mut idx = ResolverIndex::new();
        idx.add_document("DOC-B", "Roadmap"); // title "Roadmap" on DOC-B
        idx.add_alias("DOC-A", "Roadmap"); // alias "Roadmap" on DOC-A (collision)
        let r = resolve_wikilink(&idx, "roadmap");
        assert_eq!(
            r,
            WikilinkResolution::Resolved {
                document_id: "DOC-B".into(),
                matched_by: MatchKind::ExactTitle
            },
            "MC-004: a title/alias collision resolves to the EXACT TITLE (DOC-B), deterministic"
        );
    }

    #[test]
    fn normalized_duplicate_aliases_are_explicitly_ambiguous_and_stable() {
        let mut idx = ResolverIndex::new();
        idx.add_document("DOC-Z", "Zulu");
        idx.add_document("DOC-A", "Alpha");
        idx.add_alias("DOC-Z", "  Project   Atlas ");
        idx.add_alias("DOC-A", "project atlas");

        assert_eq!(
            resolve_wikilink(&idx, "PROJECT  ATLAS"),
            WikilinkResolution::Ambiguous {
                title: "PROJECT  ATLAS".into(),
                document_ids: vec!["DOC-A".into(), "DOC-Z".into()],
            }
        );
    }

    #[test]
    fn readding_same_alias_for_same_document_does_not_create_false_ambiguity() {
        let mut idx = ResolverIndex::new();
        idx.add_document("DOC-1", "Project Atlas");
        idx.add_alias("DOC-1", "Atlas");
        idx.add_alias("DOC-1", "  ATLAS  ");

        assert_eq!(
            resolve_wikilink(&idx, "atlas"),
            WikilinkResolution::Resolved {
                document_id: "DOC-1".into(),
                matched_by: MatchKind::Alias {
                    alias: "Atlas".into(),
                },
            }
        );
    }

    #[test]
    fn normalized_duplicate_titles_are_explicitly_ambiguous_and_stable() {
        let mut idx = ResolverIndex::new();
        idx.add_document("DOC-Z", "  Project   Atlas ");
        idx.add_document("DOC-A", "project atlas");

        assert_eq!(
            resolve_wikilink(&idx, "PROJECT  ATLAS"),
            WikilinkResolution::Ambiguous {
                title: "PROJECT  ATLAS".into(),
                document_ids: vec!["DOC-A".into(), "DOC-Z".into()],
            }
        );
    }

    #[test]
    fn reindexing_same_document_does_not_create_false_ambiguity_or_stale_title() {
        let mut idx = ResolverIndex::new();
        idx.add_document("DOC-1", "Old title");
        idx.add_document("DOC-1", "New title");

        assert!(matches!(
            resolve_wikilink(&idx, "Old title"),
            WikilinkResolution::Unresolved { .. }
        ));
        assert_eq!(
            resolve_wikilink(&idx, "New title").document_id(),
            Some("DOC-1")
        );
    }

    #[test]
    fn unmatched_target_is_unresolved_with_trimmed_title() {
        let idx = index();
        let r = resolve_wikilink(&idx, "  Brand New Note  ");
        assert_eq!(
            r,
            WikilinkResolution::Unresolved {
                title: "Brand New Note".into()
            },
            "an unmatched target is Unresolved carrying the trimmed original-case title"
        );
        assert!(!r.is_resolved());
        assert_eq!(r.document_id(), None);
    }

    #[test]
    fn empty_target_is_unresolved_not_a_false_match() {
        let mut idx = ResolverIndex::new();
        // A document with a blank title must NOT make every empty `[[]]` resolve.
        idx.add_document("DOC-BLANK", "   ");
        assert_eq!(
            idx.title_count(),
            0,
            "a blank title is not indexed in by_title"
        );
        let r = resolve_wikilink(&idx, "   ");
        assert!(
            matches!(r, WikilinkResolution::Unresolved { .. }),
            "empty target is unresolved"
        );
    }

    #[test]
    fn aliases_unsupported_by_default_backend_reality() {
        // AC-006: a fresh index built from the real backend payload has NO alias support and an empty
        // alias map until the local stub adds one.
        let idx = index();
        assert!(
            !idx.aliases_supported,
            "the backend payload has no aliases field (AC-006)"
        );
        assert_eq!(
            idx.alias_count(),
            0,
            "no aliases until the local stub populates them"
        );
    }

    #[test]
    fn normalize_collapses_whitespace_and_lowercases() {
        assert_eq!(normalize_target("  Foo   Bar  "), "foo bar");
        assert_eq!(normalize_target("HELLO"), "hello");
        assert_eq!(normalize_target("\t a\n b \t"), "a b");
        assert_eq!(normalize_target("   "), "");
    }

    #[test]
    fn add_alias_records_original_case_for_rendering() {
        let mut idx = ResolverIndex::new();
        idx.add_document("DOC-1", "Project Atlas");
        idx.add_alias("DOC-1", "Atlas");
        idx.add_alias("DOC-1", "ATLAS"); // case-duplicate -> not double-recorded
        let doc = idx.documents().find(|d| d.document_id == "DOC-1").unwrap();
        assert_eq!(
            doc.aliases,
            vec!["Atlas".to_string()],
            "alias original case kept, deduped case-insensitively"
        );
        assert_eq!(
            idx.alias_count(),
            1,
            "the normalized alias key collapses the case-duplicate"
        );
    }
}
