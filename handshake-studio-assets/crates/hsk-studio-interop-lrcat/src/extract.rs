#![deny(clippy::arithmetic_side_effects)]
//! Fixed Lightroom semantic extraction. This is the ONLY public decoding entry: it reads a fixed,
//! named set of Lightroom tables from granted bytes and returns typed import proposals plus
//! per-entity loss and reconciliation receipts. Decoded rows are proposals, never authority, and
//! a successful parse is not a full conversion (STU-IO-189). Table and column names below come
//! from the module brief and the LightCraft concept reference; they are UNVERIFIED against a
//! Lightroom-written catalog of any current version (validator-time oracle).
use crate::catalog_bytes::{
    Db, ENTRY_BYTES, Schema, TableDef, Value, decode_record, entry_bytes, peek_page_size,
};
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

/// Cursor step over untrusted text: saturating, and every use is guarded by a `get`/length test.
fn adv(i: usize, n: usize) -> usize {
    i.saturating_add(n)
}
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

/// Row accounting of one entity table the extractor touched, from three separate sources:
/// `rows_seen` is counted by the b-tree walker, `mapped` is the number of entries the proposal
/// (or its lookup table) holds for the entity, `lost` is the number of rows that produced no
/// entry, each recorded as a whole-row `LossNote` event. Field-level losses of a mapped row are
/// listed in `losses` but do not make the row lost. `CatalogProposal::reconciles` cross-checks
/// `rows_seen == mapped + lost` and `mapped` against the proposal content.
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
    /// Entries this proposal itself holds for an entity that has a proposal vector (`None` for
    /// lookup-only and count-only tables).
    fn produced(&self, entity: &str) -> Option<u64> {
        Some(match entity {
            "Adobe_images" => self.photos.len(),
            "Adobe_imageDevelopSettings" => self.develop.len(),
            "AgLibraryCollection" => self.collections.len(),
            "AgLibraryCollectionImage" => self.collection_members.len(),
            "AgLibraryKeyword" => self.keywords.len(),
            "AgLibraryKeywordImage" => self.keyword_assignments.len(),
            "Adobe_variablesTable" => usize::from(self.schema_version.is_some()),
            _ => return None,
        } as u64)
    }

    /// True only when the row accounting is consistent with the proposal itself: for every
    /// counted entity `rows_seen == mapped + lost`, every entity with a proposal vector has a
    /// count whose `mapped` equals the entries actually present, and the required `Adobe_images`
    /// count exists. A proposal that lost entries, lost a count or carries a skewed count does
    /// not reconcile.
    pub fn reconciles(&self) -> bool {
        const WITH_VECTOR: [&str; 7] = [
            "Adobe_images",
            "Adobe_imageDevelopSettings",
            "AgLibraryCollection",
            "AgLibraryCollectionImage",
            "AgLibraryKeyword",
            "AgLibraryKeywordImage",
            "Adobe_variablesTable",
        ];
        self.count("Adobe_images").is_some()
            && self
                .counts
                .iter()
                .all(|c| c.mapped.checked_add(c.lost) == Some(c.rows_seen))
            && WITH_VECTOR
                .iter()
                .all(|e| match (self.count(e), self.produced(e)) {
                    (Some(c), Some(n)) => c.mapped == n,
                    (None, Some(n)) => n == 0,
                    _ => true,
                })
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
        note.count = note.count.saturating_add(n);
    }
}

/// Walker row count plus the number of rows that produced no entry (`Dropped`).
#[derive(Clone, Copy)]
pub(crate) struct Walked {
    seen: u64,
    lost: u64,
}

/// Why a visited row produced no entry; `rows` records it as a whole-row loss.
pub(crate) struct Dropped {
    field: &'static str,
    disposition: Disposition,
    code: &'static str,
}

impl Dropped {
    pub(crate) const fn new(field: &'static str, disposition: Disposition, code: &'static str) -> Self {
        Self {
            field,
            disposition,
            code,
        }
    }
}

/// Root-folder path by id; folder (root id, path from root) by id; file (folder id, name) by id.
type PathTables = (
    HashMap<i64, Option<String>>,
    HashMap<i64, (Option<i64>, Option<String>)>,
    HashMap<i64, (Option<i64>, Option<String>)>,
);

pub(crate) struct Extractor<'a> {
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
    pub(crate) fn new(
        db: &'a Db<'a>,
        schema: &'a Schema,
        limits: &'a Limits,
        cancel: &'a CancellationToken,
    ) -> Self {
        Self {
            db,
            schema,
            limits,
            cancel,
            losses: RefCell::new(LossLog::default()),
            counts: RefCell::new(Vec::new()),
            retained: Cell::new(0),
        }
    }

    /// The aggregated loss notes (key order) and the entity counts in walk order.
    pub(crate) fn finish(self) -> (Vec<LossNote>, Vec<EntityCount>) {
        (
            self.losses.into_inner().map.into_values().collect(),
            self.counts.into_inner(),
        )
    }

    pub(crate) fn loss(&self, entity: &str, field: &str, d: Disposition, code: &'static str, n: u64, id: Option<i64>) {
        self.losses.borrow_mut().add(entity, field, d, code, n, id);
    }

    /// Records an entity's accounting from three separate sources: the walker's row count and
    /// whole-row loss events (`walked`) and the number of entries the caller really kept
    /// (`mapped`). `lost` is never derived by subtraction.
    pub(crate) fn count(&self, entity: &str, walked: Walked, mapped: u64) {
        self.counts.borrow_mut().push(EntityCount {
            entity: entity.to_owned(),
            rows_seen: walked.seen,
            mapped,
            lost: walked.lost,
        });
    }

    /// Keeps the first row seen for `id`; a repeated rowid (corrupt b-tree) is a whole-row loss
    /// of the later row instead of silently replacing the earlier one.
    fn put<V>(&self, map: &mut HashMap<i64, V>, id: i64, value: V) -> Result<Option<Dropped>> {
        if map.contains_key(&id) {
            return Ok(Some(Dropped {
                field: "rowid",
                disposition: Disposition::Unsupported,
                code: "duplicate_rowid",
            }));
        }
        self.db.charge(entry_bytes::<(i64, V)>())?;
        map.insert(id, value);
        Ok(None)
    }

    /// Clones retained text, charging the copy against the retained-text budget BEFORE cloning:
    /// one stored value referenced by many rows is paid for once per copy.
    fn clone_text(&self, s: &Option<String>) -> Result<Option<String>> {
        match s {
            None => Ok(None),
            Some(text) => {
                self.db.charge(text.len() as u64)?;
                Ok(Some(text.clone()))
            }
        }
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
    pub(crate) fn optional(&self, name: &'static str) -> Option<&'a TableDef> {
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

    /// Walks one table. `visit` returns `None` when the row produced its entry and `Some(Dropped)`
    /// when it produced none; every dropped row becomes a whole-row `LossNote` and one `lost`.
    pub(crate) fn rows(
        &self,
        entity: &str,
        t: &TableDef,
        names: &[&'static str],
        mut visit: impl FnMut(i64, &[Value]) -> Result<Option<Dropped>>,
    ) -> Result<Walked> {
        let want: Vec<Option<usize>> = names
            .iter()
            .map(|n| t.columns.iter().position(|c| c.eq_ignore_ascii_case(n)))
            .collect();
        let alias: Vec<bool> = want
            .iter()
            .map(|w| w.is_some() && *w == t.rowid_alias)
            .collect();
        let mut lost = 0u64;
        let seen = self.db.walk_table(t.root, &mut |cell| {
            check_cancel(self.cancel)?;
            let payload = self.db.payload(&cell)?;
            let mut vals = decode_record(&payload, self.limits.max_columns, &want)?;
            for (i, a) in alias.iter().enumerate() {
                if *a {
                    vals[i] = Value::Int(cell.rowid);
                }
            }
            if let Some(d) = visit(cell.rowid, &vals)? {
                lost = lost.saturating_add(1);
                self.loss(entity, d.field, d.disposition, d.code, 1, Some(cell.rowid));
            }
            Ok(())
        })?;
        for (i, w) in want.iter().enumerate() {
            if w.is_none() {
                self.loss(entity, names[i], Disposition::Missing, "column_absent", seen, None);
            }
        }
        Ok(Walked { seen, lost })
    }

    /// Strict text decode of a column value; kept text is charged against the retained budget.
    pub(crate) fn text(&self, entity: &str, field: &str, v: &Value, id: i64) -> Result<Option<String>> {
        if v.is_null() {
            return Ok(None);
        }
        let out = v.text(self.db.encoding);
        match &out {
            None => self.loss(entity, field, Disposition::Missing, "unexpected_or_invalid_text", 1, Some(id)),
            Some(text) => self.db.charge(text.len() as u64)?,
        }
        Ok(out)
    }

    pub(crate) fn int(&self, entity: &str, field: &str, v: &Value, id: i64) -> Option<i64> {
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
        // Counted tables are never mapped: every walked row is a whole-row loss event.
        let mut lost = 0u64;
        let seen = self.db.walk_table(t.root, &mut |_| {
            check_cancel(self.cancel)?;
            lost = lost.saturating_add(1);
            Ok(())
        })?;
        self.count(name, Walked { seen, lost }, 0);
        if lost > 0 {
            self.loss(name, "", d, code, lost, None);
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
        let walked = self.rows("Adobe_variablesTable", t, &["name", "value"], |id, v| {
            let named = self.text("Adobe_variablesTable", "name", &v[0], id)?.as_deref() == Some("Adobe_DBVersion");
            if named && version.is_none() {
                version = match &v[1] {
                    Value::Text(_) => self.text("Adobe_variablesTable", "value", &v[1], id)?,
                    Value::Int(i) => Some(i.to_string()),
                    _ => None,
                };
                if version.is_some() {
                    return Ok(None);
                }
            }
            Ok(Some(Dropped {
                field: "",
                disposition: Disposition::Unsupported,
                code: "variables_not_mapped",
            }))
        })?;
        self.count("Adobe_variablesTable", walked, u64::from(version.is_some()));
        Ok(version)
    }

    fn path_tables(&self) -> Result<PathTables> {
        let mut roots = HashMap::new();
        if let Some(t) = self.optional("AgLibraryRootFolder") {
            let walked = self.rows("AgLibraryRootFolder", t, &["absolutePath"], |id, v| {
                let path = self.text("AgLibraryRootFolder", "absolutePath", &v[0], id)?;
                self.put(&mut roots, id, path)
            })?;
            self.count("AgLibraryRootFolder", walked, roots.len() as u64);
        }
        let mut folders = HashMap::new();
        if let Some(t) = self.optional("AgLibraryFolder") {
            let walked = self.rows("AgLibraryFolder", t, &["rootFolder", "pathFromRoot"], |id, v| {
                let entry = (
                    self.int("AgLibraryFolder", "rootFolder", &v[0], id),
                    self.text("AgLibraryFolder", "pathFromRoot", &v[1], id)?,
                );
                self.put(&mut folders, id, entry)
            })?;
            self.count("AgLibraryFolder", walked, folders.len() as u64);
        }
        let mut files = HashMap::new();
        if let Some(t) = self.optional("AgLibraryFile") {
            let names = ["folder", "idx_filename", "baseName", "extension"];
            let walked = self.rows("AgLibraryFile", t, &names, |id, v| {
                let mut name = self.text("AgLibraryFile", "idx_filename", &v[1], id)?;
                if name.is_none() {
                    let base = self.text("AgLibraryFile", "baseName", &v[2], id)?;
                    let ext = self.text("AgLibraryFile", "extension", &v[3], id)?;
                    name = match base {
                        Some(b) => match ext {
                            Some(e) if !e.is_empty() => {
                                self.db.charge((b.len() as u64).saturating_add(1).saturating_add(e.len() as u64))?;
                                Some(format!("{b}.{e}"))
                            }
                            _ => Some(b),
                        },
                        None => None,
                    };
                }
                let entry = (self.int("AgLibraryFile", "folder", &v[0], id), name);
                self.put(&mut files, id, entry)
            })?;
            self.count("AgLibraryFile", walked, files.len() as u64);
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
        let walked = self.rows("Adobe_images", t, &names, |id, v| {
            let e = "Adobe_images";
            let (mut root_path, mut path_from_root, mut file_name) = (None, None, None);
            match self.int(e, "rootFile", &v[1], id).and_then(|f| files.get(&f)) {
                None => self.loss(e, "path", Disposition::Missing, "file_unresolved", 1, Some(id)),
                Some((folder, name)) => {
                    file_name = self.clone_text(name)?;
                    match folder.and_then(|f| folders.get(&f)) {
                        None => self.loss(e, "path", Disposition::Missing, "folder_unresolved", 1, Some(id)),
                        Some((root, rel)) => {
                            path_from_root = self.clone_text(rel)?;
                            match root.and_then(|r| roots.get(&r)) {
                                None => self.loss(e, "path", Disposition::Missing, "root_unresolved", 1, Some(id)),
                                Some(p) => root_path = self.clone_text(p)?,
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
            self.db.charge(entry_bytes::<PhotoProposal>())?;
            photos.push(PhotoProposal {
                local_id: id,
                global_id: self.text(e, "id_global", &v[0], id)?,
                root_path,
                path_from_root,
                file_name,
                file_format: self.text(e, "fileFormat", &v[2], id)?,
                width: self.int(e, "fileWidth", &v[3], id),
                height: self.int(e, "fileHeight", &v[4], id),
                rating,
                pick,
                color_label: self.text(e, "colorLabels", &v[7], id)?,
                capture_time: self.text(e, "captureTime", &v[8], id)?,
                orientation: self.text(e, "orientation", &v[9], id)?,
            });
            Ok(None)
        })?;
        self.count("Adobe_images", walked, photos.len() as u64);
        Ok(photos)
    }

    fn develop(&self, image_ids: &HashSet<i64>) -> Result<Vec<DevelopProposal>> {
        let mut out = Vec::new();
        let Some(t) = self.optional("Adobe_imageDevelopSettings") else {
            return Ok(out);
        };
        let e = "Adobe_imageDevelopSettings";
        let walked = self.rows(e, t, &["image", "text"], |id, v| {
            let Some(image) = v[0].as_i64().filter(|i| image_ids.contains(i)) else {
                return Ok(Some(Dropped {
                    field: "image",
                    disposition: Disposition::Unsupported,
                    code: "orphan_row",
                }));
            };
            let bytes = v[1].bytes().unwrap_or(&[]);
            if v[1].is_null() {
                self.loss(e, "text", Disposition::Missing, "settings_text_null", 1, Some(id));
            }
            let len = bytes.len() as u64;
            let sha256: [u8; 32] = Sha256::digest(bytes).into();
            let keep = self.retained.get().saturating_add(len) <= self.limits.max_retained_opaque_bytes;
            if keep {
                self.retained.set(self.retained.get().saturating_add(len));
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
            // The scalar map and the process-version copy are retained text: charge them (the
            // opaque bytes have their own budget above).
            let scalar_bytes = scalars.iter().fold(0u64, |acc, (k, val)| {
                acc.saturating_add(ENTRY_BYTES)
                    .saturating_add(k.len() as u64)
                    .saturating_add(val.len() as u64)
            });
            let version_bytes = process_version.as_ref().map_or(0, |p| p.len() as u64);
            self.db.charge(
                entry_bytes::<DevelopProposal>()
                    .saturating_add(scalar_bytes)
                    .saturating_add(version_bytes),
            )?;
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
            Ok(None)
        })?;
        self.count(e, walked, out.len() as u64);
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
            let walked = self.rows(e, t, &["name", "parent", "creationId"], |id, v| {
                let creation_id = self.text(e, "creationId", &v[2], id)?;
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
                self.db.charge(entry_bytes::<CollectionProposal>())?;
                collections.push(CollectionProposal {
                    local_id: id,
                    name: self.text(e, "name", &v[0], id)?,
                    parent: self.int(e, "parent", &v[1], id),
                    creation_id,
                    kind,
                });
                Ok(None)
            })?;
            self.count(e, walked, collections.len() as u64);
        }
        if let Some(t) = self.optional("AgLibraryCollectionImage") {
            let e = "AgLibraryCollectionImage";
            let known: HashSet<i64> = collections.iter().map(|c| c.local_id).collect();
            let walked = self.rows(e, t, &["collection", "image"], |_, v| {
                match (v[0].as_i64(), v[1].as_i64()) {
                    (Some(c), Some(i)) if known.contains(&c) && image_ids.contains(&i) => {
                        self.db.charge(entry_bytes::<CollectionMember>())?;
                        members.push(CollectionMember { collection: c, image: i });
                        Ok(None)
                    }
                    _ => Ok(Some(Dropped {
                        field: "collection,image",
                        disposition: Disposition::Unsupported,
                        code: "orphan_row",
                    })),
                }
            })?;
            self.count(e, walked, members.len() as u64);
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
            let walked = self.rows(e, t, &["name", "parent"], |id, v| {
                self.db.charge(entry_bytes::<KeywordProposal>())?;
                keywords.push(KeywordProposal {
                    local_id: id,
                    name: self.text(e, "name", &v[0], id)?,
                    parent: self.int(e, "parent", &v[1], id),
                });
                Ok(None)
            })?;
            self.count(e, walked, keywords.len() as u64);
        }
        if let Some(t) = self.optional("AgLibraryKeywordImage") {
            let e = "AgLibraryKeywordImage";
            let known: HashSet<i64> = keywords.iter().map(|k| k.local_id).collect();
            let walked = self.rows(e, t, &["image", "tag"], |_, v| {
                match (v[0].as_i64(), v[1].as_i64()) {
                    (Some(i), Some(k)) if known.contains(&k) && image_ids.contains(&i) => {
                        self.db.charge(entry_bytes::<KeywordAssignment>())?;
                        assignments.push(KeywordAssignment { image: i, keyword: k });
                        Ok(None)
                    }
                    _ => Ok(Some(Dropped {
                        field: "image,tag",
                        disposition: Disposition::Unsupported,
                        code: "orphan_row",
                    })),
                }
            })?;
            self.count(e, walked, assignments.len() as u64);
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
                unlisted = unlisted.saturating_add(1);
                continue;
            }
            listed = listed.saturating_add(1);
            let name = sanitize(&t.name);
            if let Err(e) = Self::walkable(t) {
                self.loss("unmapped_table", &name, Disposition::Unsupported, e.code(), 1, None);
                continue;
            }
            // No row of an unmapped table is mapped: each walked row is a whole-row loss event.
            let mut lost = 0u64;
            let seen = self.db.walk_table(t.root, &mut |_| {
                check_cancel(self.cancel)?;
                lost = lost.saturating_add(1);
                Ok(())
            })?;
            self.count(&format!("table:{name}"), Walked { seen, lost }, 0);
            self.loss("unmapped_table", &name, Disposition::Unsupported, "unmapped_table", lost.max(1), None);
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
        while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
            i = adv(i, 1);
        }
        i
    };
    // Index of the closing quote of a string whose body starts at `k` (or the end of the text).
    let string_end = |mut k: usize| {
        while let Some(&b) = bytes.get(k) {
            if b == b'"' {
                break;
            }
            k = adv(k, if b == b'\\' { 2 } else { 1 });
        }
        k
    };
    while i < n && out.len() < MAX_LUA_SCALARS {
        let c = bytes[i];
        if c == b'{' {
            depth = depth.saturating_add(1);
            i = adv(i, 1);
        } else if c == b'}' {
            depth = depth.saturating_sub(1);
            i = adv(i, 1);
        } else if c == b'-' && bytes.get(adv(i, 1)) == Some(&b'-') {
            while bytes.get(i).is_some_and(|b| *b != b'\n') {
                i = adv(i, 1);
            }
        } else if c == b'"' {
            i = adv(string_end(adv(i, 1)), 1);
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while bytes
                .get(i)
                .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
            {
                i = adv(i, 1);
            }
            let key = &bytes[start..i];
            let j = skip_ws(i);
            if depth != 1
                || bytes.get(j) != Some(&b'=')
                || bytes.get(adv(j, 1)) == Some(&b'=')
                || key.len() > 64
            {
                continue;
            }
            let v = skip_ws(adv(j, 1));
            let value = match bytes.get(v) {
                Some(b'{') | None => continue,
                Some(b'"') => {
                    let first = adv(v, 1);
                    let k = string_end(first);
                    let end = k.min(n);
                    i = adv(k, 1).min(n);
                    let text = bytes.get(first..end.min(adv(first, 256))).unwrap_or_default();
                    String::from_utf8_lossy(text).into_owned()
                }
                Some(_) => {
                    let mut k = v;
                    while bytes
                        .get(k)
                        .is_some_and(|b| !matches!(b, b',' | b'}' | b'\n' | b'\r' | b';'))
                    {
                        k = adv(k, 1);
                    }
                    i = k;
                    let text = bytes.get(v..k.min(adv(v, 64))).unwrap_or_default();
                    String::from_utf8_lossy(text).trim().to_owned()
                }
            };
            if !value.is_empty() {
                out.insert(String::from_utf8_lossy(key).into_owned(), value);
            }
        } else {
            i = adv(i, 1);
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
    extract_with(grant, limits, cancel, |_, _, _, _| Ok(())).map(|(p, ())| p)
}

/// The fixed extraction with an `extra` reader that runs over the same decoded database after
/// the fixed tables and BEFORE the closing fingerprint, so its reads are covered by the same
/// before/after source check. The catalog proposal does not depend on `extra` (separate loss
/// log and counts); `extra` only shares the page, cell and retained-memory budgets.
pub(crate) fn extract_with<T>(
    grant: &Grant,
    limits: &Limits,
    cancel: &CancellationToken,
    extra: impl FnOnce(&Db<'_>, &Schema, &Limits, &CancellationToken) -> Result<T>,
) -> Result<(CatalogProposal, T)> {
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
    let x = Extractor::new(&db, &schema, limits, cancel);

    let freelist = db.freelist_report()?;
    if let Some(code) = freelist.anomaly {
        x.loss("freelist", "", Disposition::Unsupported, code, 1, None);
    }
    let mut seen_names: HashSet<String> = HashSet::new();
    for t in &schema.tables {
        // The lowercase copy is a second retained copy of the (payload-bounded) table name.
        db.charge(entry_bytes::<String>().saturating_add(t.name.len() as u64))?;
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

    let extra = extra(&db, &schema, limits, cancel)?;

    let after = digest(grant, cancel)?;
    if before != after {
        return Err(LrcatError::SourceChanged);
    }
    let (losses, counts) = x.finish();
    let proposal = CatalogProposal {
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
    };
    Ok((proposal, extra))
}
