//! Fixed Lightroom semantic extraction. This is the ONLY public decoding entry: it reads a fixed,
//! named set of Lightroom tables from granted bytes and returns typed import proposals plus
//! per-entity loss and reconciliation receipts. Decoded rows are proposals, never authority, and
//! a successful parse is not a full conversion (STU-IO-189). Table and column names below come
//! from the module brief and the LightCraft concept reference; they are UNVERIFIED against a
//! Lightroom-written catalog of any current version (validator-time oracle).
use crate::catalog_bytes::{Db, Schema, TableDef, Value, decode_record, peek_page_size};
use crate::error::{LrcatError, Result, check_cancel};
use crate::source::{Grant, Limits, WalGrant};
use crate::wal_snapshot::{Fingerprint, WalOverlay, digest, scan_wal};
use hsk_studio_accord::CancellationToken;
use sha2::{Digest, Sha256};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};

pub const DECODER_ID: &str = "hsk-studio-interop-lrcat/fixed-extraction@1";
pub const CONVERSION_NOTICE: &str = "parser success and row counts are not full conversion: every unmapped, opaque or unsupported item is listed in losses";

/// Process-version strings the extractor recognises (UNVERIFIED community mapping of Camera Raw
/// `ProcessVersion` internal values; develop settings stay opaque regardless).
pub const KNOWN_PROCESS_VERSIONS: &[&str] = &["5.0", "5.7", "6.6", "6.7", "10.0", "11.0", "15.4"];
/// Catalog schema versions verified against a Lightroom-written catalog: none yet (UNVERIFIED).
pub const KNOWN_SCHEMA_VERSIONS: &[&str] = &[];

const MAX_LOSS_KEYS: usize = 4096;
const MAX_LUA_SCALARS: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Disposition {
    Editable,
    Translated,
    Approximate,
    Opaque,
    Unsupported,
    Missing,
}

impl Disposition {
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Editable => "editable",
            Self::Translated => "translated",
            Self::Approximate => "approximate",
            Self::Opaque => "opaque",
            Self::Unsupported => "unsupported",
            Self::Missing => "missing",
        }
    }
}

/// One aggregated loss: `count` occurrences of the same (entity, field, code).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LossNote {
    pub entity: String,
    pub field: String,
    pub disposition: Disposition,
    pub code: &'static str,
    pub count: u64,
    pub first_local_id: Option<i64>,
}

/// `rows_seen == mapped + lost` for every entity table the extractor touched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntityCount {
    pub entity: String,
    pub rows_seen: u64,
    pub mapped: u64,
    pub lost: u64,
}

/// Opaque bytes kept by hash and length; the bytes themselves only within the retention budget.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpaqueBlob {
    pub len: u64,
    pub sha256: [u8; 32],
    pub bytes: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PhotoProposal {
    pub local_id: i64,
    pub global_id: Option<String>,
    /// Root folder path exactly as stored in the catalog (data, not a path this crate opens).
    pub root_path: Option<String>,
    /// Folder path relative to the root folder.
    pub path_from_root: Option<String>,
    pub file_name: Option<String>,
    pub file_format: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub rating: Option<i64>,
    /// Pick flag: 1 picked, 0 none, -1 rejected.
    pub pick: Option<i64>,
    pub color_label: Option<String>,
    /// Capture time exactly as stored (not parsed).
    pub capture_time: Option<String>,
    pub orientation: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DevelopProposal {
    pub image_local_id: i64,
    pub process_version: Option<String>,
    /// The develop settings text, opaque + hashed.
    pub settings: OpaqueBlob,
    /// Top-level scalar `key = value` entries only (nested tables stay inside the opaque blob).
    pub scalars: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollectionKind {
    Standard,
    /// Rules are not decoded (loss `smart_collection_rules_not_decoded`).
    Smart,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectionProposal {
    pub local_id: i64,
    pub name: Option<String>,
    pub parent: Option<i64>,
    pub creation_id: Option<String>,
    pub kind: CollectionKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectionMember {
    pub collection: i64,
    pub image: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeywordProposal {
    pub local_id: i64,
    pub name: Option<String>,
    pub parent: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeywordAssignment {
    pub image: i64,
    pub keyword: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalReceipt {
    pub granted: bool,
    pub committed_frames: u64,
    pub ignored_tail_frames: u64,
    pub invalid_tail_bytes: u64,
    pub db_pages: Option<u32>,
}

/// Import proposal: not authority. It enters CKC/Studio only through shared ports.
#[derive(Clone, Debug, PartialEq)]
pub struct CatalogProposal {
    pub fingerprint: Fingerprint,
    pub schema_version: Option<String>,
    pub photos: Vec<PhotoProposal>,
    pub develop: Vec<DevelopProposal>,
    pub collections: Vec<CollectionProposal>,
    pub collection_members: Vec<CollectionMember>,
    pub keywords: Vec<KeywordProposal>,
    pub keyword_assignments: Vec<KeywordAssignment>,
    pub losses: Vec<LossNote>,
    pub counts: Vec<EntityCount>,
    pub wal: WalReceipt,
    pub decoder: &'static str,
    pub notice: &'static str,
}

impl CatalogProposal {
    pub fn count(&self, entity: &str) -> Option<&EntityCount> {
        self.counts.iter().find(|c| c.entity == entity)
    }
    pub fn loss(&self, entity: &str, code: &str) -> Option<&LossNote> {
        self.losses
            .iter()
            .find(|l| l.entity == entity && l.code == code)
    }
    /// Every touched entity table satisfies `rows_seen == mapped + lost`.
    pub fn reconciles(&self) -> bool {
        self.counts
            .iter()
            .all(|c| c.rows_seen == c.mapped + c.lost)
    }
    /// True only when nothing was lost, approximated, opaque or unsupported (never for a
    /// catalog that carries develop settings: those stay opaque).
    pub fn is_lossless(&self) -> bool {
        self.losses.is_empty()
    }
}

#[derive(Default)]
struct LossLog {
    map: BTreeMap<(String, String, &'static str), LossNote>,
}

impl LossLog {
    fn add(
        &mut self,
        entity: &str,
        field: &str,
        disposition: Disposition,
        code: &'static str,
        n: u64,
        id: Option<i64>,
    ) {
        let mut key = (entity.to_owned(), field.to_owned(), code);
        let mut disposition = disposition;
        if !self.map.contains_key(&key) && self.map.len() >= MAX_LOSS_KEYS {
            key = ("loss_log".to_owned(), String::new(), "loss_log_saturated");
            disposition = Disposition::Missing;
        }
        let note = self.map.entry(key.clone()).or_insert_with(|| LossNote {
            entity: key.0.clone(),
            field: key.1.clone(),
            disposition,
            code: key.2,
            count: 0,
            first_local_id: id,
        });
        note.count += n;
    }
}

/// Root-folder path by id; folder (root id, path from root) by id; file (folder id, name) by id.
type PathTables = (
    HashMap<i64, Option<String>>,
    HashMap<i64, (Option<i64>, Option<String>)>,
    HashMap<i64, (Option<i64>, Option<String>)>,
);

struct Extractor<'a> {
    db: &'a Db<'a>,
    schema: &'a Schema,
    limits: &'a Limits,
    cancel: &'a CancellationToken,
    losses: RefCell<LossLog>,
    counts: RefCell<Vec<EntityCount>>,
    retained: Cell<u64>,
}

fn sanitize(name: &str) -> String {
    name.chars().filter(char::is_ascii_graphic).take(64).collect()
}

impl<'a> Extractor<'a> {
    fn loss(&self, entity: &str, field: &str, d: Disposition, code: &'static str, n: u64, id: Option<i64>) {
        self.losses.borrow_mut().add(entity, field, d, code, n, id);
    }

    fn count(&self, entity: &str, seen: u64, mapped: u64) {
        self.counts.borrow_mut().push(EntityCount {
            entity: entity.to_owned(),
            rows_seen: seen,
            mapped,
            lost: seen.saturating_sub(mapped),
        });
    }

    fn lookup(&self, name: &str) -> Option<&'a TableDef> {
        self.schema
            .tables
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(name))
    }

    fn walkable(t: &TableDef) -> Result<()> {
        if t.root == 0 {
            Err(LrcatError::Unsupported("virtual_table"))
        } else if t.without_rowid {
            Err(LrcatError::Unsupported("without_rowid"))
        } else if !t.columns_known {
            Err(LrcatError::Corrupt("schema_sql"))
        } else {
            Ok(())
        }
    }

    fn required(&self, name: &'static str) -> Result<&'a TableDef> {
        let t = self
            .lookup(name)
            .ok_or(LrcatError::MissingRequiredTable(name))?;
        Self::walkable(t)?;
        Ok(t)
    }

    /// Optional table: absent or not walkable is a typed loss, never silence.
    fn optional(&self, name: &'static str) -> Option<&'a TableDef> {
        let Some(t) = self.lookup(name) else {
            self.loss(name, "", Disposition::Missing, "table_absent", 1, None);
            return None;
        };
        match Self::walkable(t) {
            Ok(()) => Some(t),
            Err(e) => {
                self.loss(name, "", Disposition::Unsupported, e.code(), 1, None);
                None
            }
        }
    }

    fn rows(
        &self,
        entity: &str,
        t: &TableDef,
        names: &[&'static str],
        mut visit: impl FnMut(i64, &[Value]) -> Result<()>,
    ) -> Result<u64> {
        let want: Vec<Option<usize>> = names
            .iter()
            .map(|n| t.columns.iter().position(|c| c.eq_ignore_ascii_case(n)))
            .collect();
        let alias: Vec<bool> = want
            .iter()
            .map(|w| w.is_some() && *w == t.rowid_alias)
            .collect();
        let n = self.db.walk_table(t.root, &mut |cell| {
            check_cancel(self.cancel)?;
            let payload = self.db.payload(&cell)?;
            let mut vals = decode_record(&payload, self.limits.max_columns, &want)?;
            for (i, a) in alias.iter().enumerate() {
                if *a {
                    vals[i] = Value::Int(cell.rowid);
                }
            }
            visit(cell.rowid, &vals)
        })?;
        for (i, w) in want.iter().enumerate() {
            if w.is_none() {
                self.loss(entity, names[i], Disposition::Missing, "column_absent", n, None);
            }
        }
        Ok(n)
    }

    fn text(&self, entity: &str, field: &str, v: &Value, id: i64) -> Option<String> {
        if v.is_null() {
            return None;
        }
        let out = v.text(self.db.encoding);
        if out.is_none() {
            self.loss(entity, field, Disposition::Missing, "unexpected_or_invalid_text", 1, Some(id));
        }
        out
    }

    fn int(&self, entity: &str, field: &str, v: &Value, id: i64) -> Option<i64> {
        if v.is_null() {
            return None;
        }
        let out = v.as_i64();
        if out.is_none() {
            self.loss(entity, field, Disposition::Missing, "unexpected_type", 1, Some(id));
        }
        out
    }

    fn count_only(&self, name: &'static str, d: Disposition, code: &'static str) -> Result<()> {
        let Some(t) = self.lookup(name) else {
            return Ok(());
        };
        if let Err(e) = Self::walkable(t) {
            self.loss(name, "", Disposition::Unsupported, e.code(), 1, None);
            return Ok(());
        }
        let n = self.db.walk_table(t.root, &mut |_| check_cancel(self.cancel))?;
        self.count(name, n, 0);
        if n > 0 {
            self.loss(name, "", d, code, n, None);
        }
        Ok(())
    }

    fn schema_version(&self) -> Result<Option<String>> {
        let Some(t) = self.lookup("Adobe_variablesTable") else {
            self.loss("Adobe_variablesTable", "", Disposition::Missing, "table_absent", 1, None);
            return Ok(None);
        };
        if Self::walkable(t).is_err() {
            self.loss("Adobe_variablesTable", "", Disposition::Unsupported, "table_not_walkable", 1, None);
            return Ok(None);
        }
        let mut version = None;
        let n = self.rows("Adobe_variablesTable", t, &["name", "value"], |id, v| {
            if self.text("Adobe_variablesTable", "name", &v[0], id).as_deref() == Some("Adobe_DBVersion") {
                version = match &v[1] {
                    Value::Text(_) => self.text("Adobe_variablesTable", "value", &v[1], id),
                    Value::Int(i) => Some(i.to_string()),
                    _ => None,
                };
            }
            Ok(())
        })?;
        let mapped = u64::from(version.is_some());
        self.count("Adobe_variablesTable", n, mapped);
        if n > mapped {
            self.loss("Adobe_variablesTable", "", Disposition::Unsupported, "variables_not_mapped", n - mapped, None);
        }
        Ok(version)
    }

    fn path_tables(&self) -> Result<PathTables> {
        let mut roots = HashMap::new();
        if let Some(t) = self.optional("AgLibraryRootFolder") {
            let n = self.rows("AgLibraryRootFolder", t, &["absolutePath"], |id, v| {
                roots.insert(id, self.text("AgLibraryRootFolder", "absolutePath", &v[0], id));
                Ok(())
            })?;
            self.count("AgLibraryRootFolder", n, n);
        }
        let mut folders = HashMap::new();
        if let Some(t) = self.optional("AgLibraryFolder") {
            let n = self.rows("AgLibraryFolder", t, &["rootFolder", "pathFromRoot"], |id, v| {
                folders.insert(
                    id,
                    (
                        self.int("AgLibraryFolder", "rootFolder", &v[0], id),
                        self.text("AgLibraryFolder", "pathFromRoot", &v[1], id),
                    ),
                );
                Ok(())
            })?;
            self.count("AgLibraryFolder", n, n);
        }
        let mut files = HashMap::new();
        if let Some(t) = self.optional("AgLibraryFile") {
            let names = ["folder", "idx_filename", "baseName", "extension"];
            let n = self.rows("AgLibraryFile", t, &names, |id, v| {
                let mut name = self.text("AgLibraryFile", "idx_filename", &v[1], id);
                if name.is_none() {
                    let base = self.text("AgLibraryFile", "baseName", &v[2], id);
                    let ext = self.text("AgLibraryFile", "extension", &v[3], id);
                    name = base.map(|b| match ext {
                        Some(e) if !e.is_empty() => format!("{b}.{e}"),
                        _ => b,
                    });
                }
                files.insert(id, (self.int("AgLibraryFile", "folder", &v[0], id), name));
                Ok(())
            })?;
            self.count("AgLibraryFile", n, n);
        }
        Ok((roots, folders, files))
    }

    fn photos(&self) -> Result<Vec<PhotoProposal>> {
        let (roots, folders, files) = self.path_tables()?;
        let t = self.required("Adobe_images")?;
        let names = [
            "id_global",
            "rootFile",
            "fileFormat",
            "fileWidth",
            "fileHeight",
            "rating",
            "pick",
            "colorLabels",
            "captureTime",
            "orientation",
        ];
        let mut photos = Vec::new();
        let n = self.rows("Adobe_images", t, &names, |id, v| {
            let e = "Adobe_images";
            let (mut root_path, mut path_from_root, mut file_name) = (None, None, None);
            match self.int(e, "rootFile", &v[1], id).and_then(|f| files.get(&f)) {
                None => self.loss(e, "path", Disposition::Missing, "file_unresolved", 1, Some(id)),
                Some((folder, name)) => {
                    file_name = name.clone();
                    match folder.and_then(|f| folders.get(&f)) {
                        None => self.loss(e, "path", Disposition::Missing, "folder_unresolved", 1, Some(id)),
                        Some((root, rel)) => {
                            path_from_root = rel.clone();
                            match root.and_then(|r| roots.get(&r)) {
                                None => self.loss(e, "path", Disposition::Missing, "root_unresolved", 1, Some(id)),
                                Some(p) => root_path = p.clone(),
                            }
                        }
                    }
                }
            }
            let rating = match v[5].as_f64() {
                None => {
                    if !v[5].is_null() {
                        self.loss(e, "rating", Disposition::Missing, "unexpected_type", 1, Some(id));
                    }
                    None
                }
                Some(r) if r.fract() == 0.0 && (0.0..=5.0).contains(&r) => Some(r as i64),
                Some(_) => {
                    self.loss(e, "rating", Disposition::Approximate, "rating_out_of_range", 1, Some(id));
                    None
                }
            };
            let pick = self.int(e, "pick", &v[6], id).filter(|p| {
                let ok = (-1..=1).contains(p);
                if !ok {
                    self.loss(e, "pick", Disposition::Approximate, "pick_out_of_range", 1, Some(id));
                }
                ok
            });
            photos.push(PhotoProposal {
                local_id: id,
                global_id: self.text(e, "id_global", &v[0], id),
                root_path,
                path_from_root,
                file_name,
                file_format: self.text(e, "fileFormat", &v[2], id),
                width: self.int(e, "fileWidth", &v[3], id),
                height: self.int(e, "fileHeight", &v[4], id),
                rating,
                pick,
                color_label: self.text(e, "colorLabels", &v[7], id),
                capture_time: self.text(e, "captureTime", &v[8], id),
                orientation: self.text(e, "orientation", &v[9], id),
            });
            Ok(())
        })?;
        self.count("Adobe_images", n, photos.len() as u64);
        Ok(photos)
    }

    fn develop(&self, image_ids: &HashSet<i64>) -> Result<Vec<DevelopProposal>> {
        let mut out = Vec::new();
        let Some(t) = self.optional("Adobe_imageDevelopSettings") else {
            return Ok(out);
        };
        let e = "Adobe_imageDevelopSettings";
        let n = self.rows(e, t, &["image", "text"], |id, v| {
            let Some(image) = v[0].as_i64().filter(|i| image_ids.contains(i)) else {
                self.loss(e, "image", Disposition::Unsupported, "orphan_row", 1, Some(id));
                return Ok(());
            };
            let bytes = v[1].bytes().unwrap_or(&[]);
            if v[1].is_null() {
                self.loss(e, "text", Disposition::Missing, "settings_text_null", 1, Some(id));
            }
            let len = bytes.len() as u64;
            let sha256: [u8; 32] = Sha256::digest(bytes).into();
            let keep = self.retained.get() + len <= self.limits.max_retained_opaque_bytes;
            if keep {
                self.retained.set(self.retained.get() + len);
            } else {
                self.loss(e, "text", Disposition::Opaque, "opaque_bytes_not_retained", 1, Some(id));
            }
            let scalars = scan_lua_scalars(bytes);
            let process_version = scalars.get("ProcessVersion").cloned();
            self.loss(e, "text", Disposition::Opaque, "settings_lua_opaque", 1, Some(id));
            if !process_version
                .as_deref()
                .is_some_and(|p| KNOWN_PROCESS_VERSIONS.contains(&p))
            {
                self.loss(e, "ProcessVersion", Disposition::Unsupported, "process_version_unrecognised", 1, Some(id));
            }
            if scalars.keys().any(|k| k.starts_with("Enable")) {
                self.loss(e, "Enable*", Disposition::Opaque, "enable_flags_unmapped", 1, Some(id));
            }
            out.push(DevelopProposal {
                image_local_id: image,
                process_version,
                settings: OpaqueBlob {
                    len,
                    sha256,
                    bytes: keep.then(|| bytes.to_vec()),
                },
                scalars,
            });
            Ok(())
        })?;
        self.count(e, n, out.len() as u64);
        Ok(out)
    }

    #[allow(clippy::type_complexity)]
    fn collections(
        &self,
        image_ids: &HashSet<i64>,
    ) -> Result<(Vec<CollectionProposal>, Vec<CollectionMember>)> {
        let mut collections = Vec::new();
        let mut members = Vec::new();
        if let Some(t) = self.optional("AgLibraryCollection") {
            let e = "AgLibraryCollection";
            let n = self.rows(e, t, &["name", "parent", "creationId"], |id, v| {
                let creation_id = self.text(e, "creationId", &v[2], id);
                let kind = match creation_id.as_deref() {
                    Some(c) if c.to_ascii_lowercase().contains("smart") => {
                        self.loss(e, "creationId", Disposition::Unsupported, "smart_collection_rules_not_decoded", 1, Some(id));
                        CollectionKind::Smart
                    }
                    Some("com.adobe.ag.library.collection") => CollectionKind::Standard,
                    _ => {
                        self.loss(e, "creationId", Disposition::Unsupported, "collection_kind_unmapped", 1, Some(id));
                        CollectionKind::Other
                    }
                };
                collections.push(CollectionProposal {
                    local_id: id,
                    name: self.text(e, "name", &v[0], id),
                    parent: self.int(e, "parent", &v[1], id),
                    creation_id,
                    kind,
                });
                Ok(())
            })?;
            self.count(e, n, collections.len() as u64);
        }
        if let Some(t) = self.optional("AgLibraryCollectionImage") {
            let e = "AgLibraryCollectionImage";
            let known: HashSet<i64> = collections.iter().map(|c| c.local_id).collect();
            let n = self.rows(e, t, &["collection", "image"], |id, v| {
                match (v[0].as_i64(), v[1].as_i64()) {
                    (Some(c), Some(i)) if known.contains(&c) && image_ids.contains(&i) => {
                        members.push(CollectionMember { collection: c, image: i });
                    }
                    _ => self.loss(e, "collection,image", Disposition::Unsupported, "orphan_row", 1, Some(id)),
                }
                Ok(())
            })?;
            self.count(e, n, members.len() as u64);
        }
        Ok((collections, members))
    }

    fn keywords(
        &self,
        image_ids: &HashSet<i64>,
    ) -> Result<(Vec<KeywordProposal>, Vec<KeywordAssignment>)> {
        let mut keywords = Vec::new();
        let mut assignments = Vec::new();
        if let Some(t) = self.optional("AgLibraryKeyword") {
            let e = "AgLibraryKeyword";
            let n = self.rows(e, t, &["name", "parent"], |id, v| {
                keywords.push(KeywordProposal {
                    local_id: id,
                    name: self.text(e, "name", &v[0], id),
                    parent: self.int(e, "parent", &v[1], id),
                });
                Ok(())
            })?;
            self.count(e, n, keywords.len() as u64);
        }
        if let Some(t) = self.optional("AgLibraryKeywordImage") {
            let e = "AgLibraryKeywordImage";
            let known: HashSet<i64> = keywords.iter().map(|k| k.local_id).collect();
            let n = self.rows(e, t, &["image", "tag"], |id, v| {
                match (v[0].as_i64(), v[1].as_i64()) {
                    (Some(i), Some(k)) if known.contains(&k) && image_ids.contains(&i) => {
                        assignments.push(KeywordAssignment { image: i, keyword: k });
                    }
                    _ => self.loss(e, "image,tag", Disposition::Unsupported, "orphan_row", 1, Some(id)),
                }
                Ok(())
            })?;
            self.count(e, n, assignments.len() as u64);
        }
        Ok((keywords, assignments))
    }

    fn unmapped(&self, handled: &[&str]) -> Result<()> {
        let mut listed = 0u32;
        let mut unlisted = 0u64;
        for t in &self.schema.tables {
            let lower = t.name.to_ascii_lowercase();
            if lower.starts_with("sqlite_") || handled.iter().any(|h| h.eq_ignore_ascii_case(&t.name)) {
                continue;
            }
            if listed >= self.limits.max_unmapped_tables {
                unlisted += 1;
                continue;
            }
            listed += 1;
            let name = sanitize(&t.name);
            if let Err(e) = Self::walkable(t) {
                self.loss("unmapped_table", &name, Disposition::Unsupported, e.code(), 1, None);
                continue;
            }
            let n = self.db.walk_table(t.root, &mut |_| check_cancel(self.cancel))?;
            self.count(&format!("table:{name}"), n, 0);
            self.loss("unmapped_table", &name, Disposition::Unsupported, "unmapped_table", n.max(1), None);
        }
        if unlisted > 0 {
            self.loss("unmapped_table", "", Disposition::Unsupported, "unmapped_tables_unlisted", unlisted, None);
        }
        Ok(())
    }
}

/// Scans top-level `key = scalar` entries of a develop-settings text (a Lua table literal) without
/// executing anything. Nested tables are skipped; plain identifier keys only.
pub(crate) fn scan_lua_scalars(bytes: &[u8]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let n = bytes.len();
    let (mut i, mut depth) = (0usize, 0u32);
    let skip_ws = |mut i: usize| {
        while i < n && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        i
    };
    while i < n && out.len() < MAX_LUA_SCALARS {
        let c = bytes[i];
        if c == b'{' {
            depth += 1;
            i += 1;
        } else if c == b'}' {
            depth = depth.saturating_sub(1);
            i += 1;
        } else if c == b'-' && bytes.get(i + 1) == Some(&b'-') {
            while i < n && bytes[i] != b'\n' {
                i += 1;
            }
        } else if c == b'"' {
            i += 1;
            while i < n && bytes[i] != b'"' {
                i += if bytes[i] == b'\\' { 2 } else { 1 };
            }
            i += 1;
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < n && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let key = &bytes[start..i];
            let j = skip_ws(i);
            if depth != 1 || bytes.get(j) != Some(&b'=') || bytes.get(j + 1) == Some(&b'=') || key.len() > 64 {
                continue;
            }
            let v = skip_ws(j + 1);
            let value = match bytes.get(v) {
                Some(b'{') | None => continue,
                Some(b'"') => {
                    let mut k = v + 1;
                    while k < n && bytes[k] != b'"' {
                        k += if bytes[k] == b'\\' { 2 } else { 1 };
                    }
                    let end = k.min(n);
                    i = (k + 1).min(n);
                    String::from_utf8_lossy(&bytes[v + 1..end.min(v + 1 + 256)]).into_owned()
                }
                Some(_) => {
                    let mut k = v;
                    while k < n && !matches!(bytes[k], b',' | b'}' | b'\n' | b'\r' | b';') {
                        k += 1;
                    }
                    i = k;
                    String::from_utf8_lossy(&bytes[v..k.min(v + 64)]).trim().to_owned()
                }
            };
            if !value.is_empty() {
                out.insert(String::from_utf8_lossy(key).into_owned(), value);
            }
        } else {
            i += 1;
        }
    }
    out
}

/// The only public decoding entry. Takes explicitly granted stable handles, returns a proposal or
/// nothing: any error (including cancellation) publishes no partial result and the granted bytes
/// are only ever read.
pub fn extract_catalog(
    grant: &Grant,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<CatalogProposal> {
    limits.validate()?;
    check_cancel(cancel)?;
    if matches!(grant.wal, WalGrant::PresentButNotGranted) {
        return Err(LrcatError::UngrantedWal);
    }
    let page_size = peek_page_size(grant.main.as_ref(), limits)?;
    let before = digest(grant, cancel)?;
    let overlay: Option<WalOverlay> = match &grant.wal {
        WalGrant::Granted(src) => Some(scan_wal(src.as_ref(), page_size, limits, cancel)?),
        _ => None,
    };
    let wal_arg = match (&grant.wal, &overlay) {
        (WalGrant::Granted(src), Some(o)) => Some((src.as_ref(), &o.frames, o.db_pages)),
        _ => None,
    };
    let db = Db::open(grant.main.as_ref(), wal_arg, limits, cancel)?;
    let schema = db.read_schema()?;
    let x = Extractor {
        db: &db,
        schema: &schema,
        limits,
        cancel,
        losses: RefCell::new(LossLog::default()),
        counts: RefCell::new(Vec::new()),
        retained: Cell::new(0),
    };

    let freelist = db.freelist_report()?;
    if let Some(code) = freelist.anomaly {
        x.loss("freelist", "", Disposition::Unsupported, code, 1, None);
    }
    let mut seen_names: HashSet<String> = HashSet::new();
    for t in &schema.tables {
        if !seen_names.insert(t.name.to_ascii_lowercase()) {
            x.loss("schema", &sanitize(&t.name), Disposition::Unsupported, "duplicate_table", 1, None);
        }
    }

    let schema_version = x.schema_version()?;
    match schema_version.as_deref() {
        None => x.loss("catalog", "schema_version", Disposition::Missing, "schema_version_absent", 1, None),
        Some(v) if !KNOWN_SCHEMA_VERSIONS.contains(&v) => {
            x.loss("catalog", "schema_version", Disposition::Unsupported, "schema_version_unrecognised", 1, None)
        }
        Some(_) => {}
    }
    let photos = x.photos()?;
    let image_ids: HashSet<i64> = photos.iter().map(|p| p.local_id).collect();
    let develop = x.develop(&image_ids)?;
    let (collections, collection_members) = x.collections(&image_ids)?;
    let (keywords, keyword_assignments) = x.keywords(&image_ids)?;
    x.count_only("AgLibraryCollectionContent", Disposition::Unsupported, "collection_content_not_decoded")?;
    x.count_only("Adobe_AdditionalMetadata", Disposition::Opaque, "xmp_blob_not_inflated")?;
    x.count_only("AgHarvestedExifMetadata", Disposition::Unsupported, "exif_harvest_not_mapped")?;
    x.count_only("Adobe_libraryImageDevelopHistoryStep", Disposition::Unsupported, "develop_history_not_mapped")?;
    x.count_only("Adobe_libraryImageDevelopSnapshot", Disposition::Unsupported, "develop_snapshot_not_mapped")?;
    x.unmapped(&[
        "Adobe_images",
        "Adobe_imageDevelopSettings",
        "Adobe_variablesTable",
        "AgLibraryRootFolder",
        "AgLibraryFolder",
        "AgLibraryFile",
        "AgLibraryCollection",
        "AgLibraryCollectionImage",
        "AgLibraryCollectionContent",
        "AgLibraryKeyword",
        "AgLibraryKeywordImage",
        "Adobe_AdditionalMetadata",
        "AgHarvestedExifMetadata",
        "Adobe_libraryImageDevelopHistoryStep",
        "Adobe_libraryImageDevelopSnapshot",
    ])?;

    let wal = match &overlay {
        None => WalReceipt {
            granted: false,
            committed_frames: 0,
            ignored_tail_frames: 0,
            invalid_tail_bytes: 0,
            db_pages: None,
        },
        Some(o) => WalReceipt {
            granted: true,
            committed_frames: o.committed_frames,
            ignored_tail_frames: o.ignored_tail_frames,
            invalid_tail_bytes: o.invalid_tail_bytes,
            db_pages: o.db_pages,
        },
    };
    if wal.ignored_tail_frames > 0 {
        x.loss("wal", "", Disposition::Missing, "wal_uncommitted_tail_ignored", wal.ignored_tail_frames, None);
    }
    if wal.invalid_tail_bytes > 0 {
        x.loss("wal", "", Disposition::Missing, "wal_invalid_tail_ignored", wal.invalid_tail_bytes, None);
    }

    let after = digest(grant, cancel)?;
    if before != after {
        return Err(LrcatError::SourceChanged);
    }
    let losses: Vec<LossNote> = x.losses.into_inner().map.into_values().collect();
    let counts = x.counts.into_inner();
    Ok(CatalogProposal {
        fingerprint: Fingerprint {
            main_len: before.main_len,
            wal_len: before.wal_len,
            committed_wal_len: overlay.as_ref().map_or(0, |o| o.committed_len),
            sha256_main: before.sha256_main,
            sha256_wal: before.sha256_wal,
        },
        schema_version,
        photos,
        develop,
        collections,
        collection_members,
        keywords,
        keyword_assignments,
        losses,
        counts,
        wal,
        decoder: DECODER_ID,
        notice: CONVERSION_NOTICE,
    })
}
