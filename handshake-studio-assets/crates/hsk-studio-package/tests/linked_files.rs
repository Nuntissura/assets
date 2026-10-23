//! SMB-PACKAGE-COMPLETENESS-2: the health check of linked placed sources at open
//! (`LinkedFileResolver::placed_link`): exists and hash matches, modified, missing, and hostile
//! uris refused without touching anything outside the document's directory.

use hsk_studio_folio::{self as folio, Resolution, Resolver};
use hsk_studio_package::{LinkedFileResolver, sha256_hex_of};
use std::path::PathBuf;

struct Nothing;
impl Resolver for Nothing {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Absent
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Absent
    }
    fn tile(&self, _: &folio::TileRef) -> Resolution {
        Resolution::Absent
    }
    fn format_supported(&self, _: &str) -> bool {
        false
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
    fn font(&self, _: &str, _: &str) -> Resolution {
        Resolution::Available
    }
}

struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn linked_files_are_checked_for_existence_and_hash_inside_the_document_directory() {
    let root = Dir(std::env::temp_dir().join(format!("hsk-linked-{}", std::process::id())));
    let doc_dir = root.0.join("doc");
    std::fs::create_dir_all(doc_dir.join("links")).unwrap();
    let body = b"linked source bytes".to_vec();
    std::fs::write(doc_dir.join("links/logo.psd"), &body).unwrap();
    std::fs::write(root.0.join("outside.psd"), &body).unwrap();
    let hash = sha256_hex_of(&body);
    let r = LinkedFileResolver::new(&Nothing, &doc_dir);

    assert_eq!(r.placed_link("links/logo.psd", &format!("sha256-{hash}")), Resolution::Available);
    assert_eq!(r.placed_link("links/logo.psd", &hash), Resolution::Available, "bare hex digest");
    assert_eq!(r.placed_link("links/logo.psd", &format!("sha256:{hash}")), Resolution::Available);
    assert_eq!(r.placed_link("links/logo.psd", &sha256_hex_of(b"other")), Resolution::HashMismatch, "modified");
    assert_eq!(r.placed_link("links/gone.psd", &hash), Resolution::Absent, "missing");
    assert_eq!(r.placed_link("links", &hash), Resolution::Absent, "a directory is not a file");
    assert_eq!(r.placed_link("links/logo.psd", "sha256-ab12"), Resolution::Unsupported, "a digest that is not sha256 cannot be checked");
    for hostile in ["../outside.psd", "links/../../outside.psd", "/etc/passwd", "C:/x.psd", "links\\logo.psd", "./links/logo.psd", "links//logo.psd", "", "links/\0"] {
        assert_eq!(r.placed_link(hostile, &hash), Resolution::Unauthorized, "{hostile:?}");
    }
    // The size bound: a file larger than the bound is not hashed.
    let small = LinkedFileResolver::new(&Nothing, &doc_dir).with_max_bytes(4);
    assert_eq!(small.placed_link("links/logo.psd", &hash), Resolution::Unsupported);
    // Every other port is the caller's.
    assert_eq!(r.font("Inter", "Bold"), Resolution::Available);
    assert_eq!(r.profile("SCPF-x"), Resolution::Absent);
    // A base directory that does not exist resolves nothing.
    let nowhere = LinkedFileResolver::new(&Nothing, root.0.join("nope"));
    assert_eq!(nowhere.placed_link("links/logo.psd", &hash), Resolution::Absent);
}
