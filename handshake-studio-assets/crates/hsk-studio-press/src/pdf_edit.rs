//! `PdfDoc`: the bounded PDF object view. PDF bytes are provider transport; this type is a typed
//! projection of one opened input and never a second live document authority.
use crate::error::{Limits, PressError, Result, SinkError};
use crate::reader::{Entry, Parsed, check_cancel, parse_document};
use crate::syntax::{Dict, ObjRef, Object};
use crate::writer::write_document;
use hsk_studio_accord::CancellationToken;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

static NULL: Object = Object::Null;

pub struct PdfDoc {
    pub(crate) source: Vec<u8>,
    pub(crate) version: String,
    pub(crate) objects: BTreeMap<u32, Entry>,
    pub(crate) trailer: Dict,
    pub(crate) limits: Limits,
    pub(crate) source_sha256: String,
    pub(crate) encrypted: bool,
    pub(crate) allow_modify: Option<bool>,
    pub(crate) xref_sections: usize,
    pub(crate) pages: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inspection {
    pub version: String,
    pub page_count: u32,
    pub object_count: u32,
    pub encrypted: bool,
    /// From `/Encrypt /P` bit 3; `None` when not encrypted or unreadable.
    pub permissions_allow_modify: Option<bool>,
    pub signature_count: u32,
    pub has_byte_range_signature: bool,
    /// `/SigFlags` SignaturesExist or at least one byte-range signature.
    pub signed: bool,
    /// Cross-reference sections followed (1 = never incrementally updated).
    pub xref_sections: u32,
    pub source_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PdfScalar {
    Int(i64),
    Text(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditReceipt {
    pub target: ObjRef,
    pub key: String,
    pub source_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveReceipt {
    pub bytes: usize,
    pub sha256_hex: String,
}

/// Destination for a saved artifact. `abort` must leave any previous good artifact untouched.
pub trait ArtifactSink {
    fn begin(&mut self) -> std::result::Result<(), SinkError>;
    fn write(&mut self, bytes: &[u8]) -> std::result::Result<(), SinkError>;
    fn commit(&mut self) -> std::result::Result<(), SinkError>;
    fn abort(&mut self);
}

#[derive(Clone, Debug, PartialEq)]
pub struct AnnotationView {
    pub object: ObjRef,
    pub subtype: String,
    pub rect: Option<[f64; 4]>,
    pub has_contents: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PageView {
    pub index: u32,
    pub object: ObjRef,
    pub media_box: Option<[f64; 4]>,
    pub rotate: i64,
    pub content_streams: u32,
    pub annotations: Vec<AnnotationView>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldView {
    pub object: ObjRef,
    pub name: String,
    /// `Tx`, `Btn`, `Ch`, `Sig`, or empty when absent.
    pub field_type: String,
    pub flags: i64,
    pub read_only: bool,
    pub has_value: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureView {
    pub field: ObjRef,
    pub name: String,
    pub signed: bool,
    pub byte_range_present: bool,
}

/// Typed projection proposal of the object model (pages, annotations, AcroForm fields,
/// signature states). A proposal only: nothing here mutates the document.
#[derive(Clone, Debug, PartialEq)]
pub struct ObjectModelProposal {
    pub source_sha256: String,
    pub version: String,
    pub encrypted: bool,
    pub pages: Vec<PageView>,
    pub form_fields: Vec<FieldView>,
    pub signatures: Vec<SignatureView>,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

pub(crate) fn text_string(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units = rest.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]]));
        char::decode_utf16(units).map(|r| r.unwrap_or('\u{FFFD}')).collect()
    } else if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(rest).into_owned()
    } else {
        bytes.iter().map(|b| char::from(*b)).collect()
    }
}

fn encode_text(s: &str) -> Vec<u8> {
    if s.is_ascii() {
        s.as_bytes().to_vec()
    } else {
        let mut out = vec![0xFE, 0xFF];
        for u in s.encode_utf16() {
            out.extend_from_slice(&u.to_be_bytes());
        }
        out
    }
}

impl PdfDoc {
    /// Opens and fully parses `bytes` under `limits`. Encrypted inputs open read-only (structure
    /// only); every mutation of one is a typed refusal.
    pub fn open(bytes: &[u8], limits: &Limits, cancel: &CancellationToken) -> Result<Self> {
        let Parsed { version, objects, trailer, xref_sections, encrypted } = parse_document(bytes, limits, cancel)?;
        let mut doc = Self {
            source: bytes.to_vec(),
            version,
            objects,
            trailer,
            limits: *limits,
            source_sha256: sha256_hex(bytes),
            encrypted,
            allow_modify: None,
            xref_sections,
            pages: Vec::new(),
        };
        if encrypted {
            let p = doc
                .trailer
                .get("Encrypt")
                .map(|e| doc.resolve(e))
                .and_then(Object::as_dict)
                .and_then(|d| d.get("P"))
                .and_then(Object::as_int);
            doc.allow_modify = p.map(|p| p & 4 != 0);
        } else {
            doc.pages = doc.walk_pages(cancel)?;
        }
        Ok(doc)
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    pub(crate) fn resolve<'a>(&'a self, mut o: &'a Object) -> &'a Object {
        for _ in 0..16 {
            match o {
                Object::Ref(r) => match self.objects.get(&r.num) {
                    Some(e) => o = &e.obj,
                    None => return &NULL,
                },
                _ => return o,
            }
        }
        &NULL
    }

    pub(crate) fn dict_of(&self, num: u32) -> Option<&Dict> {
        self.objects.get(&num).and_then(|e| e.obj.as_dict())
    }

    pub(crate) fn catalog(&self) -> Result<&Dict> {
        self.trailer
            .get("Root")
            .map(|r| self.resolve(r))
            .and_then(|o| match o {
                Object::Dict(d) => Some(d),
                _ => None,
            })
            .ok_or(PressError::Malformed("no_root"))
    }

    fn walk_pages(&self, cancel: &CancellationToken) -> Result<Vec<u32>> {
        let pages_ref = self
            .catalog()?
            .get("Pages")
            .and_then(Object::as_ref)
            .ok_or(PressError::Malformed("no_pages"))?;
        let mut out = Vec::new();
        let mut visited = BTreeSet::new();
        let mut stack = vec![(pages_ref.num, 1usize)];
        while let Some((num, depth)) = stack.pop() {
            check_cancel(cancel)?;
            if depth > self.limits.max_depth {
                return Err(PressError::InputLimit("depth"));
            }
            if !visited.insert(num) {
                return Err(PressError::Malformed("page_tree_cycle"));
            }
            let dict = self.dict_of(num).ok_or(PressError::Malformed("page_node"))?;
            match dict.get("Kids").map(|k| self.resolve(k)) {
                Some(Object::Array(kids)) => {
                    for kid in kids.iter().rev() {
                        let r = kid.as_ref().ok_or(PressError::Malformed("page_kid"))?;
                        stack.push((r.num, depth + 1));
                    }
                }
                Some(_) => return Err(PressError::Malformed("page_kids")),
                None => {
                    out.push(num);
                    if out.len() > self.limits.max_pages {
                        return Err(PressError::InputLimit("max_pages"));
                    }
                }
            }
        }
        Ok(out)
    }

    /// Looks up an inheritable page attribute through the `/Parent` chain.
    pub(crate) fn inherited<'a>(&'a self, page: u32, key: &str) -> Option<&'a Object> {
        let mut num = page;
        for _ in 0..self.limits.max_depth {
            let d = self.dict_of(num)?;
            if let Some(v) = d.get(key) {
                return Some(self.resolve(v));
            }
            num = d.get("Parent")?.as_ref()?.num;
        }
        None
    }

    pub(crate) fn rect_of(&self, o: Option<&Object>) -> Option<[f64; 4]> {
        let arr = self.resolve(o?).as_array()?;
        if arr.len() != 4 {
            return None;
        }
        let mut v = [0.0f64; 4];
        for (slot, item) in v.iter_mut().zip(arr) {
            *slot = self.resolve(item).as_f64()?;
        }
        Some([v[0].min(v[2]), v[1].min(v[3]), v[0].max(v[2]), v[1].max(v[3])])
    }

    fn count_signatures(&self) -> u32 {
        let mut count = 0u32;
        for entry in self.objects.values() {
            let mut stack: Vec<&Object> = vec![&entry.obj];
            while let Some(o) = stack.pop() {
                match o {
                    Object::Array(a) => stack.extend(a.iter()),
                    Object::Dict(_) | Object::Stream(_) => {
                        let d = o.as_dict().expect("dict-like");
                        if matches!(d.get("ByteRange"), Some(Object::Array(_))) && matches!(d.get("Contents"), Some(Object::Str(_))) {
                            count = count.saturating_add(1);
                        }
                        stack.extend(d.entries.iter().map(|(_, v)| v));
                    }
                    _ => {}
                }
            }
        }
        count
    }

    fn sig_flags_exist(&self) -> bool {
        self.catalog()
            .ok()
            .and_then(|c| c.get("AcroForm"))
            .map(|a| self.resolve(a))
            .and_then(Object::as_dict)
            .and_then(|a| a.get("SigFlags"))
            .map(|s| self.resolve(s))
            .and_then(Object::as_int)
            .is_some_and(|f| f & 1 != 0)
    }

    pub fn inspect(&self) -> Inspection {
        let signature_count = self.count_signatures();
        Inspection {
            version: self.version.clone(),
            page_count: self.pages.len() as u32,
            object_count: self.objects.len() as u32,
            encrypted: self.encrypted,
            permissions_allow_modify: self.allow_modify,
            signature_count,
            has_byte_range_signature: signature_count > 0,
            signed: signature_count > 0 || self.sig_flags_exist(),
            xref_sections: self.xref_sections as u32,
            source_sha256: self.source_sha256.clone(),
        }
    }

    pub(crate) fn guard_mutation(&self, cancel: &CancellationToken) -> Result<()> {
        check_cancel(cancel)?;
        if self.encrypted {
            return Err(if self.allow_modify == Some(false) { PressError::PermissionDenied } else { PressError::Encrypted });
        }
        if self.inspect().signed {
            return Err(PressError::SignedDocument);
        }
        Ok(())
    }

    fn annotation_nums(&self) -> BTreeSet<u32> {
        let mut out = BTreeSet::new();
        for page in &self.pages {
            if let Some(Object::Array(a)) = self.dict_of(*page).and_then(|d| d.get("Annots")).map(|x| self.resolve(x)) {
                out.extend(a.iter().filter_map(Object::as_ref).map(|r| r.num));
            }
        }
        out
    }

    /// Edits one allowlisted scalar entry of one dictionary object (Info `/Title /Author
    /// /Subject`, page `/Rotate`, annotation `/Contents`). Never executes actions or scripts.
    pub fn edit_entry(
        &mut self,
        target: ObjRef,
        key: &str,
        value: PdfScalar,
        expected_source_sha256: &str,
        cancel: &CancellationToken,
    ) -> Result<EditReceipt> {
        self.guard_mutation(cancel)?;
        if !expected_source_sha256.eq_ignore_ascii_case(&self.source_sha256) {
            return Err(PressError::StaleSource);
        }
        let entry = self
            .objects
            .get(&target.num)
            .filter(|e| e.generation == target.generation)
            .ok_or(PressError::NoSuchObject)?;
        if !matches!(entry.obj, Object::Dict(_)) {
            return Err(PressError::UnsupportedObject);
        }
        let is_info = self.trailer.get("Info").and_then(Object::as_ref).is_some_and(|r| r.num == target.num);
        let is_page = self.pages.contains(&target.num);
        let new_value = if is_info && matches!(key, "Title" | "Author" | "Subject") {
            match value {
                PdfScalar::Text(t) => Object::Str(encode_text(&t)),
                PdfScalar::Int(_) => return Err(PressError::KeyNotAllowed),
            }
        } else if is_page && key == "Rotate" {
            match value {
                PdfScalar::Int(v) if v % 90 == 0 => Object::Int(v),
                _ => return Err(PressError::KeyNotAllowed),
            }
        } else if key == "Contents" && self.annotation_nums().contains(&target.num) {
            match value {
                PdfScalar::Text(t) => Object::Str(encode_text(&t)),
                PdfScalar::Int(_) => return Err(PressError::KeyNotAllowed),
            }
        } else {
            return Err(PressError::KeyNotAllowed);
        };
        let entry = self.objects.get_mut(&target.num).ok_or(PressError::NoSuchObject)?;
        if let Object::Dict(d) = &mut entry.obj {
            d.set(key, new_value);
        }
        entry.span = None;
        Ok(EditReceipt { target, key: key.to_owned(), source_sha256: self.source_sha256.clone() })
    }

    /// Full save: one fresh file holding only objects reachable from the trailer.
    pub fn save_full(&self, cancel: &CancellationToken) -> Result<Vec<u8>> {
        self.guard_mutation(cancel)?;
        write_document(&self.version, &self.objects, &self.source, &self.trailer, &self.limits, cancel)
    }

    /// Saves through a sink; any fault or cancel aborts the sink so its last-good artifact
    /// stays untouched.
    pub fn save_to(&self, sink: &mut dyn ArtifactSink, cancel: &CancellationToken) -> Result<SaveReceipt> {
        let bytes = self.save_full(cancel)?;
        if sink.begin().is_err() {
            sink.abort();
            return Err(PressError::Sink(SinkError));
        }
        for chunk in bytes.chunks(64 * 1024) {
            if cancel.check().is_err() {
                sink.abort();
                return Err(PressError::Canceled);
            }
            if sink.write(chunk).is_err() {
                sink.abort();
                return Err(PressError::Sink(SinkError));
            }
        }
        if sink.commit().is_err() {
            sink.abort();
            return Err(PressError::Sink(SinkError));
        }
        Ok(SaveReceipt { bytes: bytes.len(), sha256_hex: sha256_hex(&bytes) })
    }

    /// Typed object-model projection (pages, annotations, AcroForm fields, signature states).
    pub fn proposal(&self, cancel: &CancellationToken) -> Result<ObjectModelProposal> {
        check_cancel(cancel)?;
        let mut pages = Vec::with_capacity(self.pages.len());
        for (i, num) in self.pages.iter().enumerate() {
            check_cancel(cancel)?;
            let dict = self.dict_of(*num).ok_or(PressError::Malformed("page_node"))?;
            let mut annotations = Vec::new();
            if let Some(Object::Array(a)) = dict.get("Annots").map(|x| self.resolve(x)) {
                for item in a.iter().take(self.limits.max_objects) {
                    let Some(r) = item.as_ref() else { continue };
                    let Some(ad) = self.dict_of(r.num) else { continue };
                    annotations.push(AnnotationView {
                        object: r,
                        subtype: ad.get("Subtype").and_then(|s| s.as_name().map(|n| String::from_utf8_lossy(n).into_owned())).unwrap_or_default(),
                        rect: self.rect_of(ad.get("Rect")),
                        has_contents: ad.get("Contents").is_some(),
                    });
                }
            }
            let content_streams = match dict.get("Contents").map(|c| self.resolve(c)) {
                Some(Object::Stream(_)) => 1,
                Some(Object::Array(a)) => a.len() as u32,
                _ => 0,
            };
            pages.push(PageView {
                index: i as u32,
                object: ObjRef { num: *num, generation: self.objects[num].generation },
                media_box: self.rect_of(self.inherited(*num, "MediaBox")),
                rotate: self.inherited(*num, "Rotate").and_then(Object::as_int).unwrap_or(0),
                content_streams,
                annotations,
            });
        }
        let (form_fields, signatures) = self.fields(cancel)?;
        Ok(ObjectModelProposal {
            source_sha256: self.source_sha256.clone(),
            version: self.version.clone(),
            encrypted: self.encrypted,
            pages,
            form_fields,
            signatures,
        })
    }

    fn fields(&self, cancel: &CancellationToken) -> Result<(Vec<FieldView>, Vec<SignatureView>)> {
        let mut fields = Vec::new();
        let mut sigs = Vec::new();
        let Some(form) = self
            .catalog()
            .ok()
            .and_then(|c| c.get("AcroForm"))
            .map(|a| self.resolve(a))
            .and_then(Object::as_dict)
        else {
            return Ok((fields, sigs));
        };
        let Some(Object::Array(roots)) = form.get("Fields").map(|f| self.resolve(f)) else {
            return Ok((fields, sigs));
        };
        let mut visited = BTreeSet::new();
        // (object, parent name, inherited FT, inherited Ff, depth)
        let mut stack: Vec<(ObjRef, String, String, i64, usize)> =
            roots.iter().filter_map(Object::as_ref).rev().map(|r| (r, String::new(), String::new(), 0, 1)).collect();
        while let Some((r, parent, ft, ff, depth)) = stack.pop() {
            check_cancel(cancel)?;
            if depth > self.limits.max_depth {
                return Err(PressError::InputLimit("depth"));
            }
            if !visited.insert(r.num) {
                return Err(PressError::Malformed("field_cycle"));
            }
            if visited.len() > self.limits.max_objects {
                return Err(PressError::InputLimit("max_objects"));
            }
            let Some(d) = self.dict_of(r.num) else { continue };
            let partial = d.get("T").map(|t| self.resolve(t)).and_then(|t| if let Object::Str(s) = t { Some(text_string(s)) } else { None });
            let name = match (&partial, parent.is_empty()) {
                (Some(p), true) => p.clone(),
                (Some(p), false) => format!("{parent}.{p}"),
                (None, _) => parent.clone(),
            };
            let ft = d.get("FT").map(|t| self.resolve(t)).and_then(Object::as_name).map(|n| String::from_utf8_lossy(n).into_owned()).unwrap_or(ft);
            let ff = d.get("Ff").map(|t| self.resolve(t)).and_then(Object::as_int).unwrap_or(ff);
            let kids: Vec<ObjRef> = match d.get("Kids").map(|k| self.resolve(k)) {
                Some(Object::Array(k)) => k.iter().filter_map(Object::as_ref).collect(),
                _ => Vec::new(),
            };
            let has_field_kids = kids.iter().any(|k| self.dict_of(k.num).is_some_and(|kd| kd.get("T").is_some()));
            if partial.is_some() || !has_field_kids {
                let value = d.get("V").map(|v| self.resolve(v));
                let has_value = !matches!(value, None | Some(Object::Null));
                fields.push(FieldView {
                    object: r,
                    name: name.clone(),
                    field_type: ft.clone(),
                    flags: ff,
                    read_only: ff & 1 != 0,
                    has_value,
                });
                if ft == "Sig" {
                    let byte_range_present = value.and_then(Object::as_dict).is_some_and(|v| matches!(v.get("ByteRange"), Some(Object::Array(_))));
                    sigs.push(SignatureView { field: r, name: name.clone(), signed: byte_range_present, byte_range_present });
                }
            }
            for kid in kids.iter().rev() {
                stack.push((*kid, name.clone(), ft.clone(), ff, depth + 1));
            }
        }
        Ok((fields, sigs))
    }
}
