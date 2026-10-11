//! Foundation tests for the .fig container capability. EVERY FIXTURE HERE IS SYNTHETIC, built from the
//! public community description of the format; no Figma-authored file exists on this machine, so
//! these tests prove container handling only. Editable support and Figma parity stay NOT_PROVEN.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_interop_fig::*;
use hsk_studio_observe::{
    Budget, DeliveryClass, DeliveryError, FailureCode, Observe, Outcome, SinkPort,
};
use sha2::{Digest, Sha256};

// ---------- synthetic fixture builders (test-only) ----------

fn sha(bytes: &[u8]) -> Sha256Bytes {
    Sha256::digest(bytes).into()
}

fn varuint(out: &mut Vec<u8>, mut v: u32) {
    loop {
        let b = (v & 127) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 128);
    }
}
fn kstring(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
    out.push(0);
}

/// Kiwi native type codes: bool=-1, byte=-2, int=-3, uint=-4, float=-5, string=-6.
const T_UINT: i32 = -4;
const T_STRING: i32 = -6;
/// Definition indexes in `schema_bytes`.
const D_NODE_TYPE: i32 = 0;
const D_GUID: i32 = 1;
const D_NODE_CHANGE: i32 = 2;

/// ENUM NodeType {DOCUMENT=1, CANVAS=2, FRAME=3}; STRUCT GUID {uint sessionID, uint localID};
/// MESSAGE NodeChange {GUID guid=1, NodeType type=2, string name=3}; MESSAGE Message
/// {NodeChange[] nodeChanges=1, string note=2}. Field ids and values are synthetic.
fn schema_bytes() -> Vec<u8> {
    type Def<'a> = (&'a str, u8, Vec<(&'a str, i32, bool, u32)>);
    let defs: Vec<Def> = vec![
        (
            "NodeType",
            0,
            vec![
                ("DOCUMENT", 0, false, 1),
                ("CANVAS", 0, false, 2),
                ("FRAME", 0, false, 3),
            ],
        ),
        (
            "GUID",
            1,
            vec![
                ("sessionID", T_UINT, false, 1),
                ("localID", T_UINT, false, 2),
            ],
        ),
        (
            "NodeChange",
            2,
            vec![
                ("guid", D_GUID, false, 1),
                ("type", D_NODE_TYPE, false, 2),
                ("name", T_STRING, false, 3),
            ],
        ),
        (
            "Message",
            2,
            vec![
                ("nodeChanges", D_NODE_CHANGE, true, 1),
                ("note", T_STRING, false, 2),
            ],
        ),
    ];
    let mut out = Vec::new();
    varuint(&mut out, defs.len() as u32);
    for (name, kind, fields) in defs {
        kstring(&mut out, name);
        out.push(kind);
        varuint(&mut out, fields.len() as u32);
        for (fname, ty, is_array, value) in fields {
            kstring(&mut out, fname);
            varuint(&mut out, ((ty << 1) ^ (ty >> 31)) as u32); // zigzag varint
            out.push(u8::from(is_array));
            varuint(&mut out, value);
        }
    }
    out
}

/// Message with `nodes` as (session, local, node type value, name).
fn message_bytes(nodes: &[(u32, u32, u32, &str)]) -> Vec<u8> {
    let mut out = Vec::new();
    varuint(&mut out, 1);
    varuint(&mut out, nodes.len() as u32);
    for (session, local, ty, name) in nodes {
        varuint(&mut out, 1);
        varuint(&mut out, *session);
        varuint(&mut out, *local);
        varuint(&mut out, 2);
        varuint(&mut out, *ty);
        varuint(&mut out, 3);
        kstring(&mut out, name);
        varuint(&mut out, 0);
    }
    varuint(&mut out, 2);
    kstring(&mut out, "synthetic");
    varuint(&mut out, 0);
    out
}

fn chunk(out: &mut Vec<u8>, data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
}

/// Synthetic `canvas.fig`: prelude + version + raw-deflate schema chunk + zstd message chunk.
fn canvas(prelude: &[u8; 8], version: u32, nodes: &[(u32, u32, u32, &str)]) -> Vec<u8> {
    let mut out = prelude.to_vec();
    out.extend_from_slice(&version.to_le_bytes());
    chunk(
        &mut out,
        &miniz_oxide::deflate::compress_to_vec(&schema_bytes(), 6),
    );
    chunk(
        &mut out,
        &ruzstd::encoding::compress_to_vec(
            message_bytes(nodes).as_slice(),
            ruzstd::encoding::CompressionLevel::Fastest,
        ),
    );
    out
}

fn crc32(data: &[u8]) -> u32 {
    !data.iter().fold(!0u32, |mut c, b| {
        c ^= u32::from(*b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        c
    })
}

/// ZIP writer for fixtures; names are written as given (duplicates, `..` and so on are allowed
/// so hostile containers can be built). `deflate` chooses DEFLATE over STORE per part.
fn zip(parts: &[(&str, &[u8], bool)]) -> Vec<u8> {
    let (mut out, mut central) = (Vec::new(), Vec::new());
    let p16 = |v: &mut Vec<u8>, x: u16| v.extend_from_slice(&x.to_le_bytes());
    let p32 = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
    for (name, data, deflate) in parts {
        let packed = deflate.then(|| miniz_oxide::deflate::compress_to_vec(data, 6));
        let (method, body): (u16, &[u8]) = match &packed {
            Some(d) => (8, d),
            None => (0, data),
        };
        let (crc, local) = (crc32(data), out.len() as u32);
        p32(&mut out, 0x0403_4b50);
        for v in [20, 0, method, 0, 0x0021] {
            p16(&mut out, v);
        }
        for v in [crc, body.len() as u32, data.len() as u32] {
            p32(&mut out, v);
        }
        p16(&mut out, name.len() as u16);
        p16(&mut out, 0);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(body);

        p32(&mut central, 0x0201_4b50);
        for v in [20, 20, 0, method, 0, 0x0021] {
            p16(&mut central, v);
        }
        for v in [crc, body.len() as u32, data.len() as u32] {
            p32(&mut central, v);
        }
        for v in [name.len() as u16, 0, 0, 0, 0] {
            p16(&mut central, v);
        }
        p32(&mut central, 0);
        p32(&mut central, local);
        central.extend_from_slice(name.as_bytes());
    }
    let cd_off = out.len() as u32;
    out.extend_from_slice(&central);
    p32(&mut out, 0x0605_4b50);
    for v in [0, 0, parts.len() as u16, parts.len() as u16] {
        p16(&mut out, v);
    }
    p32(&mut out, central.len() as u32);
    p32(&mut out, cd_off);
    p16(&mut out, 0);
    out
}

const SYNTH_VERSION: u32 = 7;
const THUMB: &[u8] = b"\x89PNG-synthetic-thumbnail";
const IMAGE: &[u8] = b"synthetic-image-bytes-ab12";
const EXTRA: &[u8] = b"\x00unknown\xffpart\x01";

fn nodes() -> Vec<(u32, u32, u32, &'static str)> {
    vec![
        (0, 0, 1, "Document"),
        (0, 1, 2, "Page 1"),
        (3, 42, 3, "Frame"),
        (3, 43, 3, "Frame 2"),
    ]
}

fn fig_zip(version: u32) -> Vec<u8> {
    let canvas = canvas(b"fig-kiwi", version, &nodes());
    zip(&[
        ("canvas.fig", &canvas, false),
        (
            "meta.json",
            br#"{"version":"1","file_name":"synthetic"}"#,
            true,
        ),
        ("thumbnail.png", THUMB, false),
        ("images/ab12", IMAGE, false),
        ("extra/opaque.bin", EXTRA, true),
    ])
}

fn policy() -> VersionPolicy {
    VersionPolicy::default().with_supported(FigKind::Design, SYNTH_VERSION)
}

fn import(bytes: &[u8]) -> Result<FigOutcome> {
    import_fig(
        bytes,
        &Limits::default(),
        &policy(),
        None,
        &CancellationToken::default(),
    )
}

fn imported(bytes: &[u8]) -> FigImport {
    match import(bytes).expect("synthetic container opens") {
        FigOutcome::Imported(i) => *i,
        other => panic!("expected Imported, got {other:?}"),
    }
}

// ---------- tests ----------

#[test]
fn fig_container_bounded_identity_loss() {
    let input = fig_zip(SYNTH_VERSION);
    let i = imported(&input);
    assert_eq!((i.kind, i.version), (FigKind::Design, SYNTH_VERSION));
    assert_eq!(i.container.packaging, Packaging::Zip);
    assert_eq!(i.source_sha256, sha(&input));
    let names: Vec<_> = i.container.parts.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "canvas.fig",
            "meta.json",
            "thumbnail.png",
            "images/ab12",
            "extra/opaque.bin"
        ]
    );
    // Every part is byte-exact with an independently computed sha256.
    let canvas_bytes = canvas(b"fig-kiwi", SYNTH_VERSION, &nodes());
    let expect: [(&str, &[u8]); 4] = [
        ("thumbnail.png", THUMB),
        ("images/ab12", IMAGE),
        ("extra/opaque.bin", EXTRA),
        ("canvas.fig", canvas_bytes.as_slice()),
    ];
    for (name, bytes) in expect {
        let p = i.container.part(name).expect(name);
        assert_eq!(p.bytes, bytes, "{name} byte-equal");
        assert_eq!(p.sha256, sha(bytes), "{name} sha256");
    }
    assert!(
        i.parts_sha256
            .iter()
            .any(|(n, h)| n == "extra/opaque.bin" && *h == sha(EXTRA))
    );
    // Two canvas chunks, each hashed over its data bytes.
    assert_eq!(i.container.chunks.len(), 2);
    for c in &i.container.chunks {
        assert_eq!(c.sha256, sha(i.container.chunk_bytes(c.index).unwrap()));
    }
    // The unknown extra part is preserved opaque; the canvas is reported unsupported.
    let loss = |part: &str| {
        i.loss
            .iter()
            .find(|l| l.part == part)
            .map(|l| (l.kind, l.code))
    };
    assert_eq!(
        loss("extra/opaque.bin"),
        Some((LossKind::PreservedOpaque, "unknown_part_preserved"))
    );
    assert_eq!(
        loss("canvas.fig"),
        Some((LossKind::Unsupported, "semantic_decode_blocked"))
    );
    assert_eq!(
        loss("thumbnail.png"),
        Some((LossKind::PreservedOpaque, "thumbnail_not_decoded"))
    );
    assert_eq!(
        i.container.meta,
        MetaKeys::Keys(vec!["file_name".into(), "version".into()])
    );
    assert_eq!(i.editable, EditableStatus::NotProven);
    assert_eq!(i.editable.label(), "NOT_PROVEN");
}

#[test]
fn unsupported_version_duplicate_key_bomb_cancel() {
    let canvas_ok = canvas(b"fig-kiwi", SYNTH_VERSION, &nodes());
    let limits = Limits::default();
    let live = CancellationToken::default();
    let code = |bytes: &[u8]| open_fig(bytes, &limits, &live).unwrap_err().code();

    // Unsupported version: container returned opaque-preserved, nothing decoded.
    let input = fig_zip(99);
    match import(&input).unwrap() {
        FigOutcome::OpaqueOnly { container, error } => {
            assert_eq!(
                error,
                FigError::UnsupportedVersion {
                    kind: FigKind::Design,
                    found: 99
                }
            );
            assert_eq!(container.version, 99);
            assert_eq!(container.source_sha256, sha(&input));
            assert_eq!(container.parts.len(), 5);
            assert_eq!(container.part("extra/opaque.bin").unwrap().bytes, EXTRA);
        }
        other => panic!("expected OpaqueOnly, got {other:?}"),
    }
    assert!(check_version(&open_fig(&input, &limits, &live).unwrap(), &policy()).is_err());
    // The default policy admits nothing: no real version value has been observed.
    assert!(matches!(
        import_fig(
            &fig_zip(SYNTH_VERSION),
            &limits,
            &VersionPolicy::default(),
            None,
            &live
        )
        .unwrap(),
        FigOutcome::OpaqueOnly { .. }
    ));

    // Distinct codes for hostile containers.
    let dup = zip(&[
        ("canvas.fig", &canvas_ok, false),
        ("images/ab12", b"a", false),
        ("images/ab12", b"b", false),
    ]);
    assert_eq!(code(&dup), "duplicate_entry");
    let fold = zip(&[
        ("canvas.fig", &canvas_ok, false),
        ("images/ab12", b"a", false),
        ("IMAGES/AB12", b"b", false),
    ]);
    assert_eq!(code(&fold), "duplicate_entry");
    let traversal = zip(&[("canvas.fig", &canvas_ok, false), ("../evil", b"x", false)]);
    assert_eq!(code(&traversal), "unsafe_entry_name");
    let bomb = vec![0u8; 4 << 20];
    let bomb_zip = zip(&[
        ("canvas.fig", &canvas_ok, false),
        ("images/zeros", &bomb, true),
    ]);
    assert!(bomb_zip.len() < 64 << 10, "bomb stays small on the wire");
    assert_eq!(code(&bomb_zip), "budget_exceeded");
    let few = Limits {
        max_entries: 2,
        ..Limits::default()
    };
    let three = zip(&[
        ("canvas.fig", &canvas_ok, false),
        ("a", b"1", false),
        ("b", b"2", false),
    ]);
    assert_eq!(
        open_fig(&three, &few, &live).unwrap_err(),
        FigError::BudgetExceeded("entries")
    );
    let mut overflow = canvas(b"fig-kiwi", SYNTH_VERSION, &nodes());
    let n = overflow.len();
    overflow[12..16].copy_from_slice(&(n as u32).to_le_bytes()); // first chunk longer than the file
    assert_eq!(
        code(&zip(&[("canvas.fig", &overflow, false)])),
        "chunk_overflow"
    );
    let mut truncated_header = canvas_ok.clone();
    truncated_header.extend_from_slice(&[1, 2]); // stream ends inside a chunk length
    assert_eq!(
        code(&zip(&[("canvas.fig", &truncated_header, false)])),
        "chunk_overflow"
    );
    let mut crc_bad = fig_zip(SYNTH_VERSION);
    let at = crc_bad.windows(6).position(|w| w == b"canvas").unwrap() + 20; // inside stored data
    crc_bad[at] ^= 0xff;
    assert_eq!(code(&crc_bad), "zip_invalid");
    assert_eq!(code(&zip(&[("meta.json", b"{}", false)])), "not_fig");

    // Pre-cancelled: nothing is produced.
    let dead = CancellationToken::default();
    dead.cancel();
    assert_eq!(
        open_fig(&input, &limits, &dead).unwrap_err(),
        FigError::Canceled
    );
    assert_eq!(
        import_fig(&input, &limits, &policy(), None, &dead).unwrap_err(),
        FigError::Canceled
    );

    // Observe mapping (one terminal outcome per fresh Observe): unsupported version -> Unsupported,
    // hostile -> Validation, canceled -> Canceled, success carries editable NOT_PROVEN.
    struct VecSink(Vec<(DeliveryClass, Vec<u8>)>);
    impl SinkPort for VecSink {
        fn try_send(
            &mut self,
            class: DeliveryClass,
            bytes: &[u8],
        ) -> std::result::Result<(), DeliveryError> {
            self.0.push((class, bytes.to_vec()));
            Ok(())
        }
    }
    let observe = || {
        let actor = ActorContext::new(
            "account",
            "principal",
            "owner",
            "owner-principal",
            "space",
            "session",
        )
        .unwrap();
        let resource = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap();
        Observe::new(7, 1, resource, actor, Budget::new(1, 2048).unwrap())
    };
    let emit = |result: &Result<FigOutcome>, token: &CancellationToken| {
        report(
            result,
            7,
            1,
            token,
            &mut observe(),
            &mut VecSink(Vec::new()),
        )
        .unwrap()
    };
    let r = emit(&import(&input), &live);
    assert_eq!(
        r.receipt.outcome,
        Outcome::Failure(FailureCode::Unsupported)
    );
    assert_eq!(r.editable, EditableStatus::NotProven);
    let r = emit(&import(&dup), &live);
    assert_eq!(r.receipt.outcome, Outcome::Failure(FailureCode::Validation));
    let r = emit(&import(&fig_zip(SYNTH_VERSION)), &live);
    assert_eq!(r.receipt.outcome, Outcome::Success);
    assert_eq!(r.editable, EditableStatus::NotProven);
    let r = emit(&import_fig(&input, &limits, &policy(), None, &dead), &dead);
    assert_eq!(r.receipt.outcome, Outcome::Canceled);
    let blocked = Err(FigError::EditableDecoderBlocked { reason: "x" });
    assert_eq!(
        emit(&blocked, &live).receipt.outcome,
        Outcome::Failure(FailureCode::Unavailable)
    );
}

#[test]
fn actual_fig_instance_variable_prototype_edit_reopen() {
    // No Figma-authored fixture exists: the semantic decoder is BLOCKED (NOT_PROVEN), not skipped.
    let i = imported(&fig_zip(SYNTH_VERSION));
    assert_eq!(
        decode_semantic(&i.container).unwrap_err(),
        FigError::EditableDecoderBlocked {
            reason: "NOT_PROVEN: needs original-produced semantic/render proof"
        }
    );
    assert_eq!(
        decode_semantic(&i.container).unwrap_err().code(),
        "editable_decoder_blocked"
    );

    // When the operator drops Figma-authored files at tests/fixtures/*.fig, they must open, their
    // real version must be recorded in version_map.rs, and editability stays NOT_PROVEN.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "fig"))
    {
        let bytes = std::fs::read(entry.path()).unwrap();
        let c = open_fig(&bytes, &Limits::default(), &CancellationToken::default())
            .unwrap_or_else(|e| panic!("real fixture {:?} must open: {e}", entry.path()));
        assert!(
            OBSERVED_VERSIONS
                .iter()
                .any(|(k, v, _)| *k == c.kind && *v == c.version),
            "record version {} ({}) of {:?} in version_map::OBSERVED_VERSIONS",
            c.version,
            c.kind.label(),
            entry.path()
        );
        let out = import_fig(
            &bytes,
            &Limits::default(),
            &VersionPolicy::observed(),
            None,
            &CancellationToken::default(),
        )
        .unwrap();
        let FigOutcome::Imported(imp) = out else {
            panic!("observed version must import")
        };
        assert_eq!(imp.editable, EditableStatus::NotProven);
    }
}

#[test]
fn stale_import_and_sanitized_policy() {
    let input = fig_zip(SYNTH_VERSION);
    let live = CancellationToken::default();
    let limits = Limits::default();
    let right = sha(&input);
    assert!(matches!(
        import_fig(&input, &limits, &policy(), Some(&right), &live).unwrap(),
        FigOutcome::Imported(_)
    ));
    let mut wrong = right;
    wrong[0] ^= 1;
    assert_eq!(
        import_fig(&input, &limits, &policy(), Some(&wrong), &live).unwrap_err(),
        FigError::StaleSource
    );
    // Same bytes changed after inspection: the previous identity is stale.
    let mut edited = input.clone();
    let last = edited.len() - 1;
    edited[last] ^= 1;
    assert_ne!(sha(&edited), right);

    let i = imported(&input);
    assert_eq!(
        sanitized_summary(&i, SanitizePolicy::Sanitized),
        ["meta.json", "thumbnail.png", "images/ab12"]
    );
    assert!(sanitized_summary(&i, SanitizePolicy::Preservation).is_empty());
    // The sanitized list never names canvas.fig or unknown parts: those stay opaque-preserved.
    assert!(
        !sanitized_summary(&i, SanitizePolicy::Sanitized)
            .iter()
            .any(|n| n == "canvas.fig" || n.starts_with("extra/"))
    );
}

#[test]
fn legacy_bare_and_foreign_kinds() {
    let live = CancellationToken::default();
    let limits = Limits::default();
    // Bare canvas.fig streams (no ZIP), one per classified prelude.
    for (prelude, kind) in [
        (b"fig-kiwi", FigKind::Design),
        (b"fig-jam.", FigKind::Jam),
        (b"fig-deck", FigKind::Deck),
    ] {
        let bytes = canvas(prelude, 3, &nodes());
        let c = open_fig(&bytes, &limits, &live).unwrap();
        assert_eq!((c.packaging, c.kind, c.version), (Packaging::Bare, kind, 3));
        assert_eq!(c.canvas().bytes, bytes);
        assert_eq!(c.chunks.len(), 2);
        let out = import_fig(
            &bytes,
            &limits,
            &VersionPolicy::default().with_supported(kind, 3),
            None,
            &live,
        )
        .unwrap();
        assert!(matches!(out, FigOutcome::Imported(_)));
    }
    // An unclassified prelude is never guessed. Bare: not a .fig at all. In a ZIP: opens as Unknown
    // with the bytes preserved, no chunk table, and the version is never admitted.
    let foreign = canvas(b"fig-make", 3, &nodes());
    assert_eq!(
        open_fig(&foreign, &limits, &live).unwrap_err(),
        FigError::NotFig("prelude_unknown")
    );
    let zipped = zip(&[("canvas.fig", &foreign, false)]);
    let c = open_fig(&zipped, &limits, &live).unwrap();
    assert_eq!(c.kind, FigKind::Unknown(*b"fig-make"));
    assert!(c.chunks.is_empty());
    assert_eq!(c.canvas().bytes, foreign);
    let policy = VersionPolicy::default().with_supported(c.kind, 3);
    assert!(!policy.admits(c.kind, 3));
    match import_fig(&zipped, &limits, &policy, None, &live).unwrap() {
        FigOutcome::OpaqueOnly { error, container } => {
            assert_eq!(error.code(), "unsupported_version");
            assert_eq!(container.canvas().bytes, foreign);
        }
        other => panic!("expected OpaqueOnly, got {other:?}"),
    }
    assert_eq!(
        open_fig(b"PK", &limits, &live).unwrap_err().code(),
        "not_fig"
    );
    assert_eq!(open_fig(&[], &limits, &live).unwrap_err().code(), "not_fig");
    // The descriptor is valid JSON and states the NOT_PROVEN status.
    let d: serde_json::Value = serde_json::from_str(DESCRIPTOR).unwrap();
    assert_eq!(d["status"]["editable"], "NOT_PROVEN");
}
