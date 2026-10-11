//! Real consumer of the session crate: opens a `.handshake` package file, renames/hides layers
//! through the built-in chronicle provider under revision checks, and saves atomically.
//!
//! session-consumer --descriptor
//! session-consumer --demo DIR
//! session-consumer --open IN --rename LAYER_ID NAME [--hide LAYER_ID] --save OUT [--cancel]
//!
//! `--demo` writes its own package into DIR, so DIR decides where files go (use the artifact
//! root; this example never picks a location). The resolver here grants every reference: it is
//! an example stand-in for a host catalog, not a model for production use.
use hsk_studio_accord::{ActorContext, CancellationToken};
use hsk_studio_chronicle::{
    Actor, Patch, Property, PropertyAddress, PropertyRead, PropertyValue, PropertyWrite,
};
use hsk_studio_folio::{self as folio, NodeKind, NodeRef};
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort};
use hsk_studio_package::{
    Limits, MemoryAssetSource, WriteOptions, read_package, write_package_with,
};
use hsk_studio_session::{
    CHRONICLE_PROVIDER_ID, DESCRIPTOR, ReportStatus, Session, SessionConfig, SessionPorts,
};
use serde_json::{Value, json};
use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

struct GrantAll;
impl folio::Resolver for GrantAll {
    fn primitive(&self, _: &str, _: &str) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn profile(&self, _: &str) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn tile(&self, _: &folio::TileRef) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
}

#[derive(Clone, Default)]
struct Frames(Arc<Mutex<Vec<(DeliveryClass, usize)>>>);
struct FrameSink(Frames);
impl SinkPort for FrameSink {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0
            .0
            .lock()
            .map_err(|_| DeliveryError::Unavailable)?
            .push((class, bytes.len()));
        Ok(())
    }
}

fn id(prefix: &str, n: u8) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}

fn demo_document(digest: &str) -> Value {
    let address = |n: u8, op: &str, port: &str| json!({"layer_id":id("SLYR",n),"operation_key":op,"port_key":port});
    let layer = |n: u8, kind: &str, payload: Value| json!({"schema_id":"hsk.studio.layer@1","layer_id":id("SLYR",n),"name":format!("layer-{n}"),"parent":null,"kind":kind,"payload":payload,"visible":true});
    let tile = json!({"object_key":"tile-0","layer_id":id("SLYR",2),"column":0,"row":0,"width":{"value":64,"unit":"px"},"height":{"value":64,"unit":"px"},"format":"caller-selected-format","colour_profile_id":id("SCPF",1),"schema_id":"hsk.studio.raster_tile@1","content_digest":{"algorithm":"sha256","digest":digest},"artifact_manifest_id":"caller-owned-existing-handle"});
    json!({
        "schema_id":"hsk.studio.document@1","document_id":id("SDOC",1),"revision":7,"geometry_unit":"mm","colour_mode":3,"bits_per_channel":8,
        "working_space_rgb":null,"working_space_cmyk":null,"working_space_gray":null,"working_space_spot":null,"blending_space":null,
        "artboards":[],"page_spreads":[],
        "layers":[layer(1,"group",json!({"payload_kind":"group"})),layer(2,"raster",json!({"payload_kind":"raster","tiles":[tile]}))],
        "graph":{"schema_id":"hsk.studio.layer_graph@1",
            "operations":[
                {"layer_id":id("SLYR",2),"operation_key":"source","operation_kind":"source","inputs":[],"outputs":[{"port_key":"out","value_type":"image","cardinality":"one","semantic":"source"}]},
                {"layer_id":id("SLYR",1),"operation_key":"mix","operation_kind":"composite","inputs":[{"port_key":"inputs","value_type":"image","cardinality":"many","semantic":"ordered_composite"}],"outputs":[{"port_key":"out","value_type":"image","cardinality":"one","semantic":"composite_result"}]}],
            "edges":[{"source":address(2,"source","out"),"target":address(1,"mix","inputs"),"dependency_kind":"data","input_ordinal":0}],
            "outputs":[address(1,"mix","out")]}
    })
}

fn write_demo_package(path: &Path) -> Result<(), String> {
    let mut assets = MemoryAssetSource::new();
    let digest = assets.insert((0..2048u32).map(|i| (i * 7 % 251) as u8).collect());
    let bytes = serde_json::to_vec(&demo_document(&digest.digest)).map_err(|e| e.to_string())?;
    let snapshot = match folio::inspect_bytes(
        &bytes,
        folio::Budget::default(),
        &CancellationToken::default(),
        &GrantAll,
    )
    .map_err(|d| format!("document_rejected:{:?}", d.code))?
    {
        folio::Inspection::Editable(s) => s,
        folio::Inspection::ReadOnly(_) => return Err("demo document is read-only".into()),
    };
    let mut package = Vec::new();
    write_package_with(
        &snapshot,
        &assets,
        &Default::default(),
        &WriteOptions::default(),
        &Limits::default(),
        &CancellationToken::default(),
        &mut package,
    )
    .map_err(|e| e.code().to_owned())?;
    fs::write(path, package).map_err(|e| e.to_string())
}

fn actor() -> Result<(ActorContext, Actor), String> {
    let context = ActorContext::new(
        "account",
        "principal",
        "owner",
        "owner-principal",
        "space",
        "session",
    )
    .map_err(|e| e.code().to_owned())?;
    let wire = Actor {
        account_id: "account".into(),
        principal_id: "principal".into(),
        owner_account_id: "owner".into(),
        owner_principal_id: "owner-principal".into(),
        access_space_id: "space".into(),
        session_id: "session".into(),
    };
    Ok((context, wire))
}

enum Edit {
    Rename(String, String),
    Hide(String),
}

/// Builds a chronicle patch that quotes the session's current value and property revision.
fn patch_for(session: &Session, edit: &Edit, wire: &Actor, n: u64) -> Result<Patch, String> {
    let (layer_id, property) = match edit {
        Edit::Rename(layer, _) => (layer, Property::Name),
        Edit::Hide(layer) => (layer, Property::Visible),
    };
    let layer = session
        .snapshot()
        .document()
        .layers
        .iter()
        .find(|l| &l.layer_id == layer_id)
        .ok_or("unknown layer id")?;
    let address = PropertyAddress {
        target: NodeRef {
            node_kind: NodeKind::Layer,
            node_id: layer_id.clone(),
        },
        property,
    };
    let revision = session
        .revisions()
        .iter()
        .find(|e| e.address == address)
        .ok_or("layer has no editable properties")?
        .revision;
    let (current, value) = match edit {
        Edit::Rename(_, name) => (
            PropertyValue::String(layer.name.clone()),
            PropertyValue::String(name.clone()),
        ),
        Edit::Hide(_) => (
            PropertyValue::Bool(layer.visible),
            PropertyValue::Bool(false),
        ),
    };
    Ok(Patch {
        transport_version: 1,
        document_id: session.snapshot().document().document_id.clone(),
        base_revision: session.document_revision(),
        command_id: format!("consumer-edit-{n}"),
        correlation_id: n,
        actor: wire.clone(),
        reads: vec![PropertyRead {
            address: address.clone(),
            expected_revision: revision,
            expected_value: current,
        }],
        writes: vec![PropertyWrite { address, value }],
    })
}

fn report_json(report: ReportStatus) -> Value {
    match report {
        ReportStatus::Delivered(r) => {
            json!({"delivered":true,"sequence":r.sequence,"outcome":format!("{:?}",r.outcome),"bytes":r.delivered_bytes})
        }
        ReportStatus::Undelivered(e) => json!({"delivered":false,"error":e.code()}),
        ReportStatus::Unaddressed => json!({"delivered":false,"error":"unaddressed"}),
    }
}

fn edit_and_save(
    input: &Path,
    edits: &[Edit],
    output: &Path,
    cancel: &CancellationToken,
) -> Result<Value, String> {
    let (context, wire) = actor()?;
    let frames = Frames::default();
    let ports =
        SessionPorts::new(Box::new(FrameSink(frames.clone()))).with_resolver(Box::new(GrantAll));
    let mut session = Session::open_path(input, SessionConfig::new(context), ports, cancel)
        .map_err(|f| f.code().to_owned())?;
    let opened_revision = session.document_revision();
    let mut applied = Vec::new();
    for (n, edit) in edits.iter().enumerate() {
        let patch = patch_for(&session, edit, &wire, n as u64 + 1)?;
        let params = serde_json::to_vec(&patch).map_err(|e| e.to_string())?;
        let result = session
            .submit(
                CHRONICLE_PROVIDER_ID,
                &params,
                session.document_revision(),
                cancel,
            )
            .map_err(|f| f.code().to_owned())?;
        applied.push(json!({"command":result.command_id,"previous_revision":result.previous_revision,"revision":result.revision,"changed":result.changed,"report":report_json(result.report)}));
    }
    let saved = session
        .save_to(output, cancel)
        .map_err(|f| f.code().to_owned())?;
    let frames = frames.0.lock().map_err(|_| "frames")?.len();
    Ok(json!({
        "opened_revision":opened_revision,
        "applied":applied,
        "saved":{"revision":saved.revision,"entries":saved.entries,"bytes":saved.bytes_written,"quarantined_preserved":saved.quarantined_preserved,"previews_dropped":saved.previews_dropped,"report":report_json(saved.report)},
        "operations":session.operations(),
        "observe_frames":frames,
        "providers":session.providers().ids().collect::<Vec<_>>(),
    }))
}

/// Independent read of the saved file with the package reader (not the session).
fn verify(path: &Path) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    let opened = read_package(
        &bytes,
        &Limits::default(),
        folio::Budget::default(),
        &GrantAll,
        &CancellationToken::default(),
    )
    .map_err(|e| e.code().to_owned())?;
    let snapshot = opened.snapshot().ok_or("read-only document")?;
    let document = snapshot.document();
    Ok(json!({
        "revision":document.revision,
        "layers":document.layers.iter().map(|l| json!({"id":l.layer_id,"name":l.name,"visible":l.visible})).collect::<Vec<_>>(),
        "assets":opened.assets.len(),
    }))
}

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();
    let cancel = CancellationToken::default();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["--descriptor"] => {
            println!("{DESCRIPTOR}");
            Ok(())
        }
        ["--demo", dir] => {
            let dir = PathBuf::from(dir);
            fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let path = dir.join("session-demo.handshake");
            write_demo_package(&path)?;
            let summary = edit_and_save(
                &path,
                &[
                    Edit::Rename(id("SLYR", 1), "renamed-by-session".into()),
                    Edit::Hide(id("SLYR", 2)),
                ],
                &path,
                &cancel,
            )?;
            let mut out = summary;
            out["independent_read"] = verify(&path)?;
            println!("{out}");
            Ok(())
        }
        ["--open", input, rest @ ..] => {
            let (mut edits, mut save, mut cancelled) = (Vec::new(), None, false);
            let mut flags = rest.iter();
            while let Some(flag) = flags.next() {
                match *flag {
                    "--rename" => {
                        let layer = flags.next().ok_or("missing layer id")?;
                        let name = flags.next().ok_or("missing name")?;
                        edits.push(Edit::Rename((*layer).to_owned(), (*name).to_owned()));
                    }
                    "--hide" => {
                        edits.push(Edit::Hide((*flags.next().ok_or("missing layer id")?).to_owned()))
                    }
                    "--save" => save = Some(PathBuf::from(flags.next().ok_or("missing path")?)),
                    "--cancel" => cancelled = true,
                    _ => return Err("unknown arguments".into()),
                }
            }
            if cancelled {
                cancel.cancel();
            }
            let output = save.ok_or("--save OUT required")?;
            let mut out = edit_and_save(Path::new(input), &edits, &output, &cancel)?;
            out["independent_read"] = verify(&output)?;
            println!("{out}");
            Ok(())
        }
        _ => Err("usage: --descriptor | --demo DIR | --open IN [--rename ID NAME] [--hide ID] --save OUT [--cancel]".into()),
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(2);
    }
}
