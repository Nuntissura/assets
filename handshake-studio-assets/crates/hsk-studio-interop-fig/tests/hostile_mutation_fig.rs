//! SMB-PARSER-HOSTILE-1 item 6, `.fig` target: a deterministic mutation sweep
//! (`hsk-studio-interop-tiff/tests/support/hostile_mutator.rs`) over the decoded Kiwi message of the
//! fixtures `fig-components-autolayout.fig` and `fig-basic-shapes.fig`. ZIP CRCs and the chunk
//! compression would stop raw flips, so each mutant message is rewrapped as `tests/fig_import.rs`
//! does (the schema chunk kept, the message zstd-compressed with ruzstd, the container rewritten
//! STORE-only) and runs `import_fig`, `decode_document`, `to_native`, `plan_frame_exports` and
//! `export_frame_pdf` of the first top-level frame, with `Limits::default()` and
//! `Budget::default()`. Not included: the synthetic `fig_import.rs` `fig_zip` family (its schema
//! builder is private to that file).
mod common;
#[path = "../../hsk-studio-interop-tiff/tests/support/hostile_mutator.rs"]
mod hostile_mutator;

use common::{Host, fixture};
use hostile_mutator::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::Budget;
use hsk_studio_interop_fig::*;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

const CAP: usize = 256 << 20;

/// A container whose `canvas.fig` message can be replaced.
struct Wrapper {
    /// Every part in order, `canvas.fig` included (replaced on rewrap).
    parts: Vec<(String, Vec<u8>)>,
    canvas_at: usize,
    /// Prelude and version (12 bytes).
    head: Vec<u8>,
    schema_chunk: Vec<u8>,
    /// Chunks after the message, verbatim.
    tail_chunks: Vec<Vec<u8>>,
    message: Vec<u8>,
}

fn chunk(out: &mut Vec<u8>, data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
}

fn unwrap(name: &str) -> Wrapper {
    let bytes = fixture(name);
    let cancel = CancellationToken::default();
    let FigOutcome::Imported(import) = import_fig(&bytes, &Limits::default(), &VersionPolicy::observed(), None, &cancel).expect("fixture imports") else {
        panic!("{name} imports")
    };
    let c = &import.container;
    let canvas_at = c.parts.iter().position(|p| p.name == "canvas.fig").expect("canvas.fig");
    let canvas = &c.parts[canvas_at].bytes;
    let data = |i: usize| -> Vec<u8> {
        let k = &c.chunks[i];
        canvas[k.offset..k.offset + k.len].to_vec()
    };
    let schema_chunk = data(0);
    let raw = data(1);
    let message = if raw.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        let mut out = Vec::new();
        let mut src = raw.as_slice();
        let mut dec = ruzstd::decoding::StreamingDecoder::new(&mut src).expect("zstd");
        std::io::Read::read_to_end(&mut dec, &mut out).expect("zstd body");
        out
    } else {
        miniz_oxide::inflate::decompress_to_vec(&raw).expect("deflate message")
    };
    Wrapper {
        parts: c.parts.iter().map(|p| (p.name.clone(), p.bytes.clone())).collect(),
        canvas_at,
        head: canvas[..12].to_vec(),
        schema_chunk,
        tail_chunks: (2..c.chunks.len()).map(data).collect(),
        message,
    }
}

/// A STORE-only ZIP of `parts` (after `tests/fig_import.rs` `zip`).
fn zip(parts: &[(String, Vec<u8>)]) -> Vec<u8> {
    let (mut out, mut central) = (Vec::new(), Vec::new());
    let p16 = |v: &mut Vec<u8>, x: u16| v.extend_from_slice(&x.to_le_bytes());
    let p32 = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
    for (name, data) in parts {
        let (crc, local) = (hsk_studio_package::crc32_of(data), out.len() as u32);
        p32(&mut out, 0x0403_4b50);
        for v in [20, 0, 0, 0, 0x0021] {
            p16(&mut out, v);
        }
        for v in [crc, data.len() as u32, data.len() as u32] {
            p32(&mut out, v);
        }
        p16(&mut out, name.len() as u16);
        p16(&mut out, 0);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        p32(&mut central, 0x0201_4b50);
        for v in [20, 20, 0, 0, 0, 0x0021] {
            p16(&mut central, v);
        }
        for v in [crc, data.len() as u32, data.len() as u32] {
            p32(&mut central, v);
        }
        for v in [name.len() as u16, 0, 0, 0, 0] {
            p16(&mut central, v);
        }
        p32(&mut central, 0);
        p32(&mut central, local);
        central.extend_from_slice(name.as_bytes());
    }
    let (cd_off, cd_len) = (out.len() as u32, central.len() as u32);
    out.extend_from_slice(&central);
    p32(&mut out, 0x0605_4b50);
    for v in [0, 0, parts.len() as u16, parts.len() as u16] {
        p16(&mut out, v);
    }
    p32(&mut out, cd_len);
    p32(&mut out, cd_off);
    p16(&mut out, 0);
    out
}

impl Wrapper {
    fn rewrap(&self, message: &[u8]) -> Vec<u8> {
        let mut canvas = self.head.clone();
        chunk(&mut canvas, &self.schema_chunk);
        chunk(&mut canvas, &ruzstd::encoding::compress_to_vec(message, ruzstd::encoding::CompressionLevel::Fastest));
        for c in &self.tail_chunks {
            chunk(&mut canvas, c);
        }
        let mut parts = self.parts.clone();
        parts[self.canvas_at].1 = canvas;
        zip(&parts)
    }
}

fn answer(file: &[u8]) -> Answer {
    let cancel = CancellationToken::default();
    let limits = Limits::default();
    let code = |e: FigError| match e {
        FigError::BudgetExceeded(w) | FigError::Zip(w) | FigError::SchemaInvalid(w) | FigError::MessageInvalid(w) => w,
        other => other.code(),
    };
    let import = match import_fig(file, &limits, &VersionPolicy::observed(), None, &cancel).map_err(code)? {
        FigOutcome::Imported(i) => i,
        FigOutcome::OpaqueOnly { error, .. } => return Err(code(error)),
    };
    let doc = decode_document(&import, &limits, &cancel).map_err(code)?;
    let p = to_native(&doc, &NativeConfig { budget: Budget::default(), resolver: &Host }, &mut SequentialIdMint::new(1), &cancel).map_err(code)?;
    let Some(frame) = doc.nodes.iter().find(|n| n.type_name == "FRAME" && n.parent.and_then(|q| doc.node(q)).is_some_and(|q| q.type_name == "CANVAS")) else {
        return Err("no_top_level_frame");
    };
    let (plans, _) = plan_frame_exports(&doc, &p, frame.id);
    let Some(plan) = plans.first() else { return Err("no_export_plan") };
    export_frame_pdf(&p, plan, None, &cancel).map(|_| ()).map_err(code)
}

const PROFILE: Profile<'static> = Profile { dictionary: &[b"\x00", b"\xFF\xFF\xFF\xFF\x0F", b"\x80\x80\x80\x80\x08", b"FRAME\x00", b"INSTANCE\x00", b"\x01\x01"], text: false };

/// `min_ok`: half the ok answers the first run measured (lane notes).
fn run(target: &'static str, name: &str, seed_const: u64, min_ok: usize) {
    let w = unwrap(name);
    assert_eq!(answer(&w.rewrap(&w.message)), Ok(()), "{name}: the rewrapped seed exports");
    let seeds = vec![w.message.clone()];
    let report = sweep(target, seed_const, &seeds, 250, PROFILE, CAP, |m| answer(&w.rewrap(m)));
    report.assert_reach(min_ok);
}

#[test]
fn fig_mutation_sweep_components_autolayout() {
    run("fig_components_autolayout", "fig-components-autolayout.fig", 0xF160_0000_0000_0001, 13);
}

#[test]
fn fig_mutation_sweep_basic_shapes() {
    run("fig_basic_shapes", "fig-basic-shapes.fig", 0xF160_0000_0000_0002, 11);
}
