//! Visual/code/DOM/template/responsive workflow over one source revision and shared editor/preview
//! mechanics. No separate app/server/job or persistence authority.
#![forbid(unsafe_code)]

mod assets;
mod css;
mod dom;
mod entity;
mod error;
mod preview;
mod refresh;
mod report;
pub mod responsive;
pub mod source_dom_patch;

pub use assets::{AssetDecision, AssetOutcome, AssetPort, AssetRef, AssetSource, DenyReason, collect_assets};
pub use css::{Declaration, RuleRefusal};
pub use dom::{
    Attr, ContentModel, Descendants, Limits, Node, NodeId, NodeKey, NodeKind, Quote, ViewRow, WebDoc, parse, sha256_hex,
};
pub use entity::{Decoded, decode_entities, escape_attr, escape_text};
pub use error::{
    EditRefusal, LimitKind, LossEntry, LossKind, LossReport, MAX_LOSS_ENTRIES, MalformedKind, UnsupportedKind, WebError,
};
pub use preview::{ActiveRule, HiddenReason, PreviewNode, PreviewTree, UnsupportedRule, UnsupportedRuleReason, preview_tree};
pub use refresh::{KeyMap, MAX_REPORTED_ROOTS, key_map, refresh};
pub use report::{outcome_of, report};
pub use source_dom_patch::{
    Applied, SourcePatch, TextEdit, apply, insert_element, insert_fragment, remove_attribute, remove_node, set_attribute, set_text,
};

pub const DESCRIPTOR: &str = r#"{"schema":"hsk.studio.web.descriptor@1","crate":"hsk-studio-web","module":"STUDIO-MODULE-WEB","authority":"source text is the authority; WebDoc is a span-preserving projection of one source revision, never a second document, server, job or persistence authority","api":["parse","WebDoc::view","WebDoc::key_of","WebDoc::resolve","WebDoc::text_content","set_attribute","remove_attribute","set_text","remove_node","insert_element","insert_fragment","apply","WebDoc::apply_patch","responsive::parse_media_query","responsive::scan_media_blocks","responsive::document_media_blocks","decode_entities","refresh","key_map","collect_assets","preview_tree","report","outcome_of"],"html_subset":{"elements":"explicit end tags only","attributes":["double-quoted","single-quoted","unquoted","valueless"],"nodes":["element","text","comment","doctype"],"void":["area","base","br","col","embed","hr","img","input","link","meta","param","source","track","wbr"],"raw_text_kept_opaque":["script","style","iframe","noembed","noframes","xmp","textarea","title"],"foreign":"svg and math parsed with the same grammar plus self-closing tags","opaque_with_loss_entry":["cdata","processing_instruction","bogus_markup"],"entities":"common named set plus numeric; unknown well-formed references left verbatim"},"patch":{"unit":"TextEdit span replacement","binding":"base revision + base sha256","guarantee":"bytes outside edited spans stay identical; unedited round trip is byte-exact","errors":["StaleRevision","Conflict","OverlappingEdits","InvalidSpan"]},"responsive":{"media":["min-width px","max-width px","and","comma list","all","screen","only"],"unsupported_typed":["media types other than all/screen","other features","em/rem and other units","range syntax","not/or"]},"refresh":{"contract":"re-parse as exactly revision+1; KeyMap old->new ids, added and removed subtree roots","identity":"prefix/suffix then in-order by tag+html id, then tag alone; ids survive reordering inside one parent"},"assets":{"classes":["Granted","Unavailable","Denied(external_scheme|protocol_relative|file_scheme|script_scheme|not_granted)","Inline(data:)","SameDocument(#)"],"port_called_for":"relative references only","scanned":["img/source/video/audio/iframe/embed/track/input src","link/a/area/use href","object data","video poster","style attribute url()","style body url() and @import string"],"not_scanned":["srcset","image-set()","meta refresh"],"io":"none"},"preview":{"per_node":["role","accessible_name","focusable","tab_index","rendered","hidden","a11y_hidden"],"tab_order":"positive tabindex ascending then natural; negative and disabled skipped","style":"inline style tags only; media-gated; simple selectors tag/*/.class/#id compounds and lists; display resolved by importance, origin, specificity, order","unsupported_listed":["at-rules","combinators/attribute/pseudo selectors","nested blocks","unsupported media queries"],"not_modelled":["layout","full cascade","name-from-content excludes no hidden descendants"],"carries":["source_revision","viewport_width_px"]},"observe":"report() emits Success|Canceled|Failure(Validation|Unsupported) with correlation id and revision only; never source text","limits_default":{"max_bytes":8388608,"max_nodes":200000,"max_depth":128,"max_attributes":256,"max_edits":1024,"max_rules":2048},"refusals":["Canceled","LimitExceeded","Malformed","Unsupported","NoSuchNode","NoSuchAttribute","InvalidName","InvalidSpan","OverlappingEdits","NotEditable","Conflict","StaleRevision","RevisionOverflow","InvalidViewport"],"never":["execute script","fetch assets","network","file I/O","guess optional end tags","re-nest misnested tags"]}"#;
