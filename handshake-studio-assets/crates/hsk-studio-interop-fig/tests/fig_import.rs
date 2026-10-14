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
    schema_with(2)
}

/// `schema_bytes` with the GUID struct's `localID` field value chosen by the caller (1 duplicates the
/// `sessionID` value).
fn schema_with(local_id_value: u32) -> Vec<u8> {
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
                ("localID", T_UINT, false, local_id_value),
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
                ("fileVersion", T_UINT, false, 18),
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

/// Synthetic `canvas.fig` around raw message bytes: prelude + version + raw-deflate schema chunk +
/// zstd message chunk.
fn canvas_with_message(prelude: &[u8; 8], version: u32, message: &[u8]) -> Vec<u8> {
    let mut out = prelude.to_vec();
    out.extend_from_slice(&version.to_le_bytes());
    chunk(
        &mut out,
        &miniz_oxide::deflate::compress_to_vec(&schema_bytes(), 6),
    );
    chunk(
        &mut out,
        &ruzstd::encoding::compress_to_vec(message, ruzstd::encoding::CompressionLevel::Fastest),
    );
    out
}

/// Synthetic `canvas.fig` with exactly the given schema and message chunk bytes.
fn canvas_chunks(schema_chunk: &[u8], message_chunk: &[u8]) -> Vec<u8> {
    let mut out = b"fig-kiwi".to_vec();
    out.extend_from_slice(&SYNTH_VERSION.to_le_bytes());
    chunk(&mut out, schema_chunk);
    chunk(&mut out, message_chunk);
    out
}

fn canvas(prelude: &[u8; 8], version: u32, nodes: &[(u32, u32, u32, &str)]) -> Vec<u8> {
    canvas_with_message(prelude, version, &message_bytes(nodes))
}

fn put_u16(bytes: &mut [u8], at: usize, v: u16) {
    bytes[at..at + 2].copy_from_slice(&v.to_le_bytes());
}
fn put_u32(bytes: &mut [u8], at: usize, v: u32) {
    bytes[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
/// Offsets of every occurrence of a 4-byte ZIP signature.
fn find_all(bytes: &[u8], sig: [u8; 4]) -> Vec<usize> {
    bytes
        .windows(4)
        .enumerate()
        .filter(|(_, w)| *w == sig)
        .map(|(i, _)| i)
        .collect()
}

/// ZIP writer for fixtures; names are written as given (duplicates, `..` and so on are allowed
/// so hostile containers can be built). `deflate` chooses DEFLATE over STORE per part.
fn zip(parts: &[(&str, &[u8], bool)]) -> Vec<u8> {
    zip_junk(parts, &[])
}

/// `zip` with `junk` appended to every DEFLATE body (inside the declared compressed size).
fn zip_junk(parts: &[(&str, &[u8], bool)], junk: &[u8]) -> Vec<u8> {
    let (mut out, mut central) = (Vec::new(), Vec::new());
    let p16 = |v: &mut Vec<u8>, x: u16| v.extend_from_slice(&x.to_le_bytes());
    let p32 = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
    for (name, data, deflate) in parts {
        let packed = deflate.then(|| {
            let mut body = miniz_oxide::deflate::compress_to_vec(data, 6);
            body.extend_from_slice(junk);
            body
        });
        let (method, body): (u16, &[u8]) = match &packed {
            Some(d) => (8, d),
            None => (0, data),
        };
        let (crc, local) = (hsk_studio_package::crc32_of(data), out.len() as u32);
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
        (u32::MAX, 70_000, 1, "Second document"),
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

    // Kiwi schema + census: ids are the file's own values, formatted "session:local", never reminted.
    let schema = i.schema_summary.as_ref().expect("schema summary");
    assert_eq!(schema.chunk_index, 0);
    assert_eq!(schema.chunk_sha256, i.container.chunks[0].sha256);
    let names: Vec<_> = schema
        .definitions
        .iter()
        .map(|d| (d.name.as_str(), d.kind))
        .collect();
    assert_eq!(
        names,
        [
            ("NodeType", "enum"),
            ("GUID", "struct"),
            ("NodeChange", "message"),
            ("Message", "message")
        ]
    );
    let node_changes = &schema.definitions[3].fields[0];
    assert_eq!(
        (
            node_changes.name.as_str(),
            node_changes.id,
            node_changes.type_name.as_str(),
            node_changes.is_array
        ),
        ("nodeChanges", 1, "NodeChange", true)
    );
    let census = i.census.as_ref().expect("census");
    assert_eq!(census.node_count, 5);
    assert_eq!(
        census
            .node_type_histogram
            .iter()
            .map(|(k, v)| (k.as_str(), *v))
            .collect::<Vec<_>>(),
        [("CANVAS", 1), ("DOCUMENT", 2), ("FRAME", 2)]
    );
    assert_eq!(
        census
            .root_ids
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["0:0", "4294967295:70000"]
    );
    assert_eq!(
        loss("canvas.fig#chunk0"),
        Some((LossKind::PreservedOpaque, "schema_chunk_summarized"))
    );
    assert_eq!(
        loss("canvas.fig#chunk1"),
        Some((LossKind::PreservedOpaque, "message_chunk_census_only"))
    );
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

    // Local headers must agree with the central directory (a reader that trusts the local header
    // would otherwise see a different entry): each mismatch is refused with its own typed reason.
    let base = zip(&[
        ("canvas.fig", &canvas_ok, false),
        ("meta.json", br#"{"a":1}"#, true),
    ]);
    let locals = find_all(&base, [0x50, 0x4b, 3, 4]);
    assert_eq!(locals.len(), 2, "fixture has exactly two local headers");
    let patched = |entry: usize, edit: &dyn Fn(&mut [u8], usize)| {
        let mut bytes = base.clone();
        edit(&mut bytes, locals[entry]);
        open_fig(&bytes, &limits, &live)
    };
    let zip_err = FigError::Zip;
    // Local header fields: flags +6, method +8, crc +14, compressed +18, uncompressed +22.
    assert_eq!(
        patched(0, &|b, at| put_u16(b, at + 8, 8)).unwrap_err(),
        zip_err("local_method_mismatch")
    );
    assert_eq!(
        patched(1, &|b, at| put_u16(b, at + 8, 0)).unwrap_err(),
        zip_err("local_method_mismatch")
    );
    assert_eq!(
        patched(0, &|b, at| b[at + 14] ^= 1).unwrap_err(),
        zip_err("local_header_mismatch")
    );
    assert_eq!(
        patched(0, &|b, at| b[at + 18] ^= 1).unwrap_err(),
        zip_err("local_header_mismatch")
    );
    assert_eq!(
        patched(1, &|b, at| b[at + 22] ^= 1).unwrap_err(),
        zip_err("local_header_mismatch")
    );
    assert_eq!(
        patched(0, &|b, at| put_u16(b, at + 6, 1)).unwrap_err(),
        zip_err("encrypted")
    );
    // A data descriptor (flag bit 3) may defer CRC and sizes: all-zero local fields are accepted and
    // the central values are used; a descriptor flag with wrong non-zero values is still a mismatch.
    let defer = |b: &mut [u8], at: usize| {
        put_u16(b, at + 6, 8);
        b[at + 14..at + 26].fill(0);
    };
    let reference = open_fig(&base, &limits, &live).unwrap();
    assert_eq!(patched(0, &defer).unwrap().parts, reference.parts);
    assert_eq!(
        patched(1, &|b, at| {
            put_u16(b, at + 6, 8);
            b[at + 14] ^= 1;
        })
        .unwrap_err(),
        zip_err("local_header_mismatch")
    );

    // u32::MAX sizes and offsets are typed refusals, never wrapped arithmetic.
    let centrals = find_all(&base, [0x50, 0x4b, 1, 2]);
    assert_eq!(centrals.len(), 2);
    for field in [20, 24, 42] {
        let mut bytes = base.clone();
        put_u32(&mut bytes, centrals[0] + field, u32::MAX);
        assert_eq!(
            open_fig(&bytes, &limits, &live).unwrap_err(),
            zip_err("zip64_unsupported"),
            "central field +{field} = u32::MAX"
        );
    }
    let mut chunk_max = canvas_ok.clone();
    put_u32(&mut chunk_max, 12, u32::MAX);
    assert_eq!(
        code(&zip(&[("canvas.fig", &chunk_max, false)])),
        "chunk_overflow"
    );
    assert_eq!(code(&zip(&[("meta.json", b"{}", false)])), "not_fig");

    // Kiwi decode failures never yield a partial census: a message the schema cannot walk (unknown
    // field id) is a soft loss entry on an otherwise safe import; a bound hit is a hard error.
    let bad_message = canvas_with_message(b"fig-kiwi", SYNTH_VERSION, &[9, 0]);
    let soft = imported(&zip(&[("canvas.fig", &bad_message, false)]));
    assert!(soft.schema_summary.is_some() && soft.census.is_none());
    assert!(soft.loss.iter().any(|l| l.part == "canvas.fig"
        && l.kind == LossKind::Unsupported
        && l.code == "unknown_field_id"));
    let truncated = canvas_with_message(b"fig-kiwi", SYNTH_VERSION, &message_bytes(&nodes())[..9]);
    let soft = imported(&zip(&[("canvas.fig", &truncated, false)]));
    assert!(soft.census.is_none());
    let hard =
        |l: &Limits| import_fig(&fig_zip(SYNTH_VERSION), l, &policy(), None, &live).unwrap_err();
    assert_eq!(
        hard(&Limits {
            max_message_bytes: 16,
            ..Limits::default()
        }),
        FigError::BudgetExceeded("message_bytes")
    );
    assert_eq!(
        hard(&Limits {
            max_schema_bytes: 8,
            ..Limits::default()
        }),
        FigError::BudgetExceeded("schema_bytes")
    );
    assert_eq!(
        hard(&Limits {
            max_nodes: 2,
            ..Limits::default()
        }),
        FigError::BudgetExceeded("nodes")
    );
    assert_eq!(
        hard(&Limits {
            max_values: 3,
            ..Limits::default()
        }),
        FigError::BudgetExceeded("values")
    );
    assert_eq!(
        hard(&Limits {
            max_definitions: 2,
            ..Limits::default()
        }),
        FigError::BudgetExceeded("definitions")
    );
    // Trailing bytes after a compressed stream end are refused with a typed reason, never ignored
    // (RFC 1951 gives a DEFLATE stream a final block, so bytes after it carry no data; RFC 8878 s3.1
    // allows concatenated zstd frames, but only the first frame is decoded here, so a census of it
    // would be partial). The clean deflate message is the no-false-positive control.
    let schema_deflated = miniz_oxide::deflate::compress_to_vec(&schema_bytes(), 6);
    let message = message_bytes(&nodes());
    let message_deflated = miniz_oxide::deflate::compress_to_vec(&message, 6);
    let message_zstd = ruzstd::encoding::compress_to_vec(
        message.as_slice(),
        ruzstd::encoding::CompressionLevel::Fastest,
    );
    let decode_loss = |imp: &FigImport| {
        imp.loss
            .iter()
            .find(|l| {
                l.part == "canvas.fig"
                    && l.kind == LossKind::Unsupported
                    && l.code != "semantic_decode_blocked"
            })
            .map(|l| l.code)
    };
    let decoded = |chunks: (&[u8], &[u8])| {
        imported(&zip(&[(
            "canvas.fig",
            &canvas_chunks(chunks.0, chunks.1),
            false,
        )]))
    };
    let clean = decoded((&schema_deflated, &message_deflated));
    assert!(clean.census.is_some() && decode_loss(&clean).is_none());
    let junk = |bytes: &[u8]| [bytes, &[0u8][..]].concat();
    let case = decoded((&junk(&schema_deflated), &message_deflated));
    assert!(case.schema_summary.is_none() && case.census.is_none());
    assert_eq!(decode_loss(&case), Some("schema_chunk_trailing_bytes"));
    let case = decoded((&schema_deflated, &junk(&message_deflated)));
    assert!(case.schema_summary.is_some() && case.census.is_none());
    assert_eq!(decode_loss(&case), Some("message_chunk_trailing_bytes"));
    let case = decoded((&schema_deflated, &junk(&message_zstd)));
    assert!(case.census.is_none());
    assert_eq!(decode_loss(&case), Some("zstd_trailing_bytes"));
    let two_frames = [message_zstd.clone(), message_zstd.clone()].concat();
    let case = decoded((&schema_deflated, &two_frames));
    assert!(case.census.is_none());
    assert_eq!(decode_loss(&case), Some("zstd_trailing_bytes"));
    let entry_junk = zip_junk(
        &[
            ("canvas.fig", &canvas_ok, false),
            ("meta.json", b"{}", true),
        ],
        &[0],
    );
    assert_eq!(
        open_fig(&entry_junk, &limits, &live).unwrap_err(),
        FigError::Zip("trailing_compressed_bytes")
    );

    // Duplicate field ids inside a struct are a typed schema refusal at parse time (the GUID struct
    // below gives sessionID and localID the same id), never last-wins.
    let dup_schema = miniz_oxide::deflate::compress_to_vec(&schema_with(1), 6);
    let case = decoded((&dup_schema, &message_zstd));
    assert!(case.schema_summary.is_none() && case.census.is_none());
    assert_eq!(decode_loss(&case), Some("duplicate_field_id"));

    // A schema whose type index is out of range is a soft, typed loss.
    let mut bad_schema = vec![1u8];
    bad_schema.extend_from_slice(b"M\0");
    bad_schema.extend_from_slice(&[2, 1]);
    bad_schema.extend_from_slice(b"f\0");
    bad_schema.extend_from_slice(&[10, 0, 1]); // zigzag(5) = definition 5 of 1
    let mut canvas_bad_schema = b"fig-kiwi".to_vec();
    canvas_bad_schema.extend_from_slice(&SYNTH_VERSION.to_le_bytes());
    chunk(
        &mut canvas_bad_schema,
        &miniz_oxide::deflate::compress_to_vec(&bad_schema, 6),
    );
    let soft = imported(&zip(&[("canvas.fig", &canvas_bad_schema, false)]));
    assert!(soft.schema_summary.is_none() && soft.census.is_none());
    assert!(
        soft.loss
            .iter()
            .any(|l| l.part == "canvas.fig" && l.code == "type_out_of_range")
    );

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

// ---------- real Figma-authored fixtures (Operator, Figma desktop 126.10.7, tests/fixtures) ----------

const REAL_FIXTURES: [&str; 3] = [
    "fig-basic-shapes.fig",
    "fig-components-autolayout.fig",
    "fig-prototype-pages.fig",
];

fn real_fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("fixture {name} must be checked in: {e}"))
}

fn real_import(name: &str) -> FigImport {
    let out = import_fig(
        &real_fixture(name),
        &Limits::default(),
        &VersionPolicy::observed(),
        None,
        &CancellationToken::default(),
    )
    .unwrap_or_else(|e| panic!("real fixture {name} must import: {e}"));
    let FigOutcome::Imported(imp) = out else {
        panic!("observed version must import: {name}")
    };
    *imp
}

#[test]
fn real_fig_fixtures_container_facts() {
    let provenance: serde_json::Value =
        serde_json::from_slice(&real_fixture("provenance.json")).unwrap();
    assert_eq!(provenance["author"], "Operator");
    let listed = provenance["files"].as_array().unwrap();
    for name in REAL_FIXTURES {
        let bytes = real_fixture(name);
        let entry = listed.iter().find(|f| f["name"] == name).unwrap();
        assert_eq!(
            entry["sha256"],
            hex(&sha(&bytes)),
            "{name} provenance sha256"
        );
        assert_eq!(entry["size"], bytes.len(), "{name} provenance size");

        let c = open_fig(&bytes, &Limits::default(), &CancellationToken::default()).unwrap();
        assert_eq!(c.packaging, Packaging::Zip);
        assert_eq!(c.kind, FigKind::Design);
        assert_eq!(c.version, 106, "header u32 LE after the 8-byte prelude");
        assert_eq!(&c.canvas().bytes[..8], b"fig-kiwi");
        assert!(c.part(META_PART).is_some() && c.part(THUMBNAIL_PART).is_some());
        // Chunk table: u32 LE length prefixes, exactly two chunks, no bytes left over.
        assert_eq!(c.chunks.len(), 2);
        let end = c.chunks[1].offset + c.chunks[1].len;
        assert_eq!(end, c.canvas().bytes.len());
        // chunk 1 is a zstd frame; chunk 0 (schema) is raw DEFLATE, proven by the schema summary.
        assert!(
            c.chunk_bytes(1)
                .unwrap()
                .starts_with(&[0x28, 0xB5, 0x2F, 0xFD])
        );
        assert!(
            !c.chunk_bytes(0)
                .unwrap()
                .starts_with(&[0x28, 0xB5, 0x2F, 0xFD])
        );

        let imp = real_import(name);
        let schema = imp.schema_summary.as_ref().expect("schema decodes");
        assert_eq!(schema.chunk_index, 0);
        assert_eq!(schema.definitions.len(), 653);
        let census = imp.census.as_ref().expect("zstd message decodes fully");
        // Message.fileVersion (field 18) is absent from these files: recorded as None, not invented.
        assert_eq!(census.file_version, None);
        assert_eq!(
            census.root_ids,
            vec![SourceId {
                session: 0,
                local: 0
            }]
        );
        assert_eq!(census.node_type_histogram["DOCUMENT"], 1);
    }
    assert!(
        OBSERVED_VERSIONS
            .iter()
            .any(|(k, v, _)| *k == FigKind::Design && *v == 106)
    );
    assert_eq!(
        OBSERVED_VERSIONS.len(),
        1,
        "only observed versions are listed"
    );
    assert!(VersionPolicy::observed().admits(FigKind::Design, 106));
    assert!(!VersionPolicy::observed().admits(FigKind::Design, 105));
    assert!(!VersionPolicy::observed().admits(FigKind::Jam, 106));
}

#[test]
fn real_fig_node_change_schema_field_count_pinned() {
    let imp = real_import("fig-basic-shapes.fig");
    let defs = &imp.schema_summary.as_ref().unwrap().definitions;
    let node = defs.iter().find(|d| d.name == "NodeChange").unwrap();
    // Field count of the schema embedded in the real files (research/app-inspect-figma.md: 616).
    assert_eq!(node.fields.len(), 616);
    assert_eq!(node.fields.iter().map(|f| f.id).max(), Some(616));
    let max = Limits::default().max_fields;
    assert!(
        node.fields.len() < max,
        "NodeChange must fit max_fields {max}"
    );
    // The headroom is thin enough to notice: a limit one below the real count refuses the file.
    let tight = Limits {
        max_fields: 615,
        ..Limits::default()
    };
    let out = import_fig(
        &real_fixture("fig-basic-shapes.fig"),
        &tight,
        &VersionPolicy::observed(),
        None,
        &CancellationToken::default(),
    );
    assert_eq!(out.unwrap_err(), FigError::BudgetExceeded("fields"));
}

fn diff_canvas() -> Vec<u8> {
    let mut b = b"fig-diff".to_vec();
    b.extend_from_slice(&7u32.to_le_bytes());
    b.extend_from_slice(&[1, 2, 3]);
    b
}

#[test]
fn fig_diff_is_known_unsupported_and_file_version_is_recorded() {
    // `fig-diff` (installed editor constant): typed kind, not NotFig, never decoded.
    let bare = diff_canvas();
    let zipped = zip(&[("canvas.fig", &diff_canvas(), false)]);
    for bytes in [bare, zipped] {
        let c = open_fig(&bytes, &Limits::default(), &CancellationToken::default()).unwrap();
        assert_eq!(c.kind, FigKind::Diff);
        assert_eq!(c.kind.prelude(), *b"fig-diff");
        assert_eq!(c.kind.label(), "diff");
        assert!(c.chunks.is_empty());
        let admitting = VersionPolicy::default().with_supported(FigKind::Diff, 7);
        let out = import_fig(
            &bytes,
            &Limits::default(),
            &admitting,
            None,
            &CancellationToken::default(),
        )
        .unwrap();
        let FigOutcome::OpaqueOnly { error, container } = out else {
            panic!("fig-diff must stay opaque")
        };
        assert_eq!(
            error,
            FigError::UnsupportedKind {
                kind: FigKind::Diff
            }
        );
        assert_eq!(error.code(), "unsupported_kind");
        assert_eq!(error.failure_code(), Some(FailureCode::Unsupported));
        assert_eq!(container.kind, FigKind::Diff);
    }
    // Message.fileVersion (field 18) lands in the census when present.
    let mut msg = message_bytes(&nodes());
    assert_eq!(msg.pop(), Some(0));
    varuint(&mut msg, 18);
    varuint(&mut msg, 106);
    varuint(&mut msg, 0);
    let with_version = zip(&[(
        "canvas.fig",
        &canvas_with_message(b"fig-kiwi", SYNTH_VERSION, &msg),
        false,
    )]);
    assert_eq!(
        imported(&with_version).census.unwrap().file_version,
        Some(106)
    );
    assert_eq!(
        imported(&fig_zip(SYNTH_VERSION))
            .census
            .unwrap()
            .file_version,
        None
    );
}

#[test]
fn actual_fig_instance_variable_prototype_edit_reopen() {
    // Synthetic container: the semantic decoder is BLOCKED (NOT_PROVEN), not skipped.
    let i = imported(&fig_zip(SYNTH_VERSION));
    assert_eq!(
        decode_semantic(&i.container).unwrap_err(),
        FigError::EditableDecoderBlocked {
            reason: "NOT_PROVEN: needs original-produced semantic/render proof"
        }
    );

    // Real Figma-authored files: the census (node types, source ids, NodeChange field presence) and,
    // since SMB-INTEROP-FIG-NODES, the typed node tree (tests/fig_nodes.rs) decode; nothing is mapped
    // into the Studio native document yet, so editable import stays NOT_PROVEN on every real file.
    let count = |imp: &FigImport, ty: &str| {
        imp.census
            .as_ref()
            .unwrap()
            .node_type_histogram
            .get(ty)
            .copied()
            .unwrap_or(0)
    };
    let field = |imp: &FigImport, name: &str| {
        imp.census
            .as_ref()
            .unwrap()
            .node_field_histogram
            .get(name)
            .copied()
            .unwrap_or(0)
    };
    let shapes = real_import("fig-basic-shapes.fig");
    for ty in [
        "ELLIPSE",
        "ROUNDED_RECTANGLE",
        "STAR",
        "LINE",
        "VECTOR",
        "BOOLEAN_OPERATION",
        "TEXT",
        "FRAME",
    ] {
        assert!(count(&shapes, ty) > 0, "basic shapes: {ty}");
    }
    let comps = real_import("fig-components-autolayout.fig");
    assert_eq!(count(&comps, "SYMBOL"), 2);
    assert_eq!(count(&comps, "INSTANCE"), 3);
    assert_eq!(count(&comps, "VARIABLE"), 3);
    assert_eq!(count(&comps, "VARIABLE_SET"), 1);
    assert_eq!(field(&comps, "symbolData"), 3);
    assert_eq!(field(&comps, "stackMode"), 8);
    assert_eq!(
        field(&comps, "variableSetModes"),
        1,
        "one variable set carries its modes list"
    );
    let proto = real_import("fig-prototype-pages.fig");
    assert_eq!(count(&proto, "CANVAS"), 3);
    assert_eq!(field(&proto, "prototypeInteractions"), 3);
    assert_eq!(field(&proto, "prototypeStartingPoint"), 1);

    for name in REAL_FIXTURES {
        let imp = real_import(name);
        assert_eq!(imp.editable, EditableStatus::NotProven);
        assert_eq!(
            decode_semantic(&imp.container).unwrap_err().code(),
            "editable_decoder_blocked"
        );
        eprintln!(
            "NOT_PROVEN: actual_fig_instance_variable_prototype_edit_reopen file={name} decoded=census+typed_node_tree editable_import=not_mapped(no native-document mapping, no edit/reopen round trip)"
        );
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
