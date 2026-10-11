//! Visual/code/DOM/template/responsive workflow over one source revision and shared editor/preview
//! mechanics. No separate app/server/job or persistence authority.
#![forbid(unsafe_code)]

mod dom;
mod entity;
mod error;
pub mod responsive;
pub mod source_dom_patch;

pub use dom::{
    Attr, ContentModel, Descendants, Limits, Node, NodeId, NodeKey, NodeKind, Quote, ViewRow, WebDoc, parse, sha256_hex,
};
pub use entity::{Decoded, decode_entities, escape_attr, escape_text};
pub use error::{
    EditRefusal, LimitKind, LossEntry, LossKind, LossReport, MAX_LOSS_ENTRIES, MalformedKind, UnsupportedKind, WebError,
};
pub use source_dom_patch::{
    Applied, SourcePatch, TextEdit, apply, insert_element, insert_fragment, remove_attribute, remove_node, set_attribute, set_text,
};

pub const DESCRIPTOR: &str = r#"{"schema":"hsk.studio.web.descriptor@1","crate":"hsk-studio-web","module":"STUDIO-MODULE-WEB","authority":"source text is the authority; WebDoc is a span-preserving projection of one source revision, never a second document, server, job or persistence authority","api":["parse","WebDoc::view","WebDoc::key_of","WebDoc::resolve","WebDoc::text_content","set_attribute","remove_attribute","set_text","remove_node","insert_element","insert_fragment","apply","WebDoc::apply_patch","responsive::parse_media_query","responsive::scan_media_blocks","responsive::document_media_blocks","decode_entities"],"html_subset":{"elements":"explicit end tags only","attributes":["double-quoted","single-quoted","unquoted","valueless"],"nodes":["element","text","comment","doctype"],"void":["area","base","br","col","embed","hr","img","input","link","meta","param","source","track","wbr"],"raw_text_kept_opaque":["script","style","iframe","noembed","noframes","xmp","textarea","title"],"foreign":"svg and math parsed with the same grammar plus self-closing tags","opaque_with_loss_entry":["cdata","processing_instruction","bogus_markup"],"entities":"common named set plus numeric; unknown well-formed references left verbatim"},"patch":{"unit":"TextEdit span replacement","binding":"base revision + base sha256","guarantee":"bytes outside edited spans stay identical; unedited round trip is byte-exact","errors":["StaleRevision","Conflict","OverlappingEdits","InvalidSpan"]},"responsive":{"media":["min-width px","max-width px","and","comma list","all","screen","only"],"unsupported_typed":["media types other than all/screen","other features","em/rem and other units","range syntax","not/or"]},"limits_default":{"max_bytes":8388608,"max_nodes":200000,"max_depth":128,"max_attributes":256,"max_edits":1024},"refusals":["Canceled","LimitExceeded","Malformed","Unsupported","NoSuchNode","NoSuchAttribute","InvalidName","InvalidSpan","OverlappingEdits","NotEditable","Conflict","StaleRevision","RevisionOverflow"],"never":["execute script","fetch assets","network","file I/O","guess optional end tags","re-nest misnested tags"]}"#;
