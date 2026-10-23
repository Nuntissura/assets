//! SMB-PACKAGE-COMPLETENESS-2: Folio's optional catalog `asset_id` on media clips and placed
//! sources (`hsk_studio_folio::is_catalog_asset_id`) accepts exactly what the CKC port's
//! `AssetId::new` accepts (`Asset.asset_id: UUID`, SMB-CKC-PORTS-ALIGN), so a document's catalog
//! reference is always a valid port identity and the port never sees a document id it refuses.

use hsk_studio_folio as folio;
use hsk_studio_kernel_bindings::AssetId;

#[test]
fn folio_catalog_asset_ids_are_exactly_the_ckc_port_asset_ids() {
    let mut corpus: Vec<String> = vec![
        "0190f5c2-7a1b-7c3d-8e4f-000000000abc".into(),
        "00000000-0000-0000-0000-000000000000".into(),
        "ffffffff-ffff-ffff-ffff-ffffffffffff".into(),
        "0190F5C2-7A1B-7C3D-8E4F-000000000ABC".into(),
        "0190f5c27a1b7c3d8e4f000000000abc".into(),
        "{0190f5c2-7a1b-7c3d-8e4f-000000000abc}".into(),
        "0190f5c2-7a1b-7c3d-8e4f-000000000abc ".into(),
        " 0190f5c2-7a1b-7c3d-8e4f-000000000abc".into(),
        "0190f5c2_7a1b_7c3d_8e4f_000000000abc".into(),
        "0190f5c2-7a1b-7c3d-8e4f-000000000ab".into(),
        "0190f5c2-7a1b-7c3d-8e4f-000000000abcd".into(),
        "0190f5c2-7a1b-7c3d-8e4f-00000000gabc".into(),
        "urn:uuid:0190f5c2-7a1b-7c3d-8e4f-000000000abc".into(),
        String::new(),
        "SAST-019abcde-0000-7000-8000-000000000001".into(),
        "0190f5c2-7a1b-7c3d-8e4f-00000000０abc".into(),
    ];
    // Every byte at every position of a valid id, so the two grammars are compared exhaustively
    // on single-byte mutations.
    let base = "0190f5c2-7a1b-7c3d-8e4f-000000000abc";
    for at in 0..base.len() {
        for byte in 0u8..=127 {
            let mut v = base.as_bytes().to_vec();
            v[at] = byte;
            if let Ok(s) = String::from_utf8(v) {
                corpus.push(s);
            }
        }
    }
    let mut accepted = 0;
    for s in &corpus {
        let port = AssetId::new(s).is_ok();
        assert_eq!(folio::is_catalog_asset_id(s), port, "{s:?}");
        accepted += usize::from(port);
    }
    assert!(accepted > 3 && accepted < corpus.len(), "the corpus has both sides: {accepted}/{}", corpus.len());
}
