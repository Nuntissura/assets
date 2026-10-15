//! Pulse-local real consumer of `plan_timeline_edit`: builds a small Folio document (25 fps,
//! V1 [clip1 0..3f, clip2 3f..6f, clip3 8f..11f], sync-locked A1 [clip4 0..2f, clip5 8f..10f],
//! every source 10 frames long), plans ONE typed edit and prints the proposal as JSON on stdout.
//! A refusal prints a stable error code on stderr and exits 2. Nothing is committed.
//!
//! `pulse-timeline-consumer [--descriptor] [--op OP] [--clip N] [--frames N] [--ripple]
//!  [--new-id N] [--from F --to T] [--track N] [--cancel]` with OP one of trim-out (default,
//!  `--frames` = new duration), trim-in (`--frames` = new head), razor (`--frames` = cut point,
//!  `--new-id` = right-half clip number), roll, slip, slide (signed `--frames`), lift, extract,
//!  lift-range, extract-range (`--from/--to` frames, `--new-id` for a straddled clip),
//!  close-gap (`--track`, `--frames` = a tick inside the gap).
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId, Ticks};
use hsk_studio_folio::{
    Budget, ClipSchema, ClipSource, ContentDigest, DocumentSchema, GeometryUnit, GraphSchema,
    ProfileBinding, Resolution, Resolver, SequenceFrameRate, SequenceSchema, Speed, StudioClip,
    StudioDocument, StudioLayerGraph, StudioSequence, StudioTrack, TileRef, TrackKind, TrackSchema,
    validate_document,
};
use hsk_studio_observe::{
    Budget as ObserveBudget, DeliveryClass, DeliveryError, Observe, SinkPort,
};
use hsk_studio_pulse::*;
use std::env;

const F: u64 = 10_160_640_000;

struct Available;
impl Resolver for Available {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Available
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Available
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        true
    }
    fn media(&self, _: &str, _: &ContentDigest) -> Resolution {
        Resolution::Available
    }
}

/// Every source is ten frames long.
struct TenFrames;
impl SourceExtents for TenFrames {
    fn extent(&self, _: &ClipSource) -> Option<Ticks> {
        Some(Ticks::new(10 * F))
    }
}

struct CollectSink(Vec<Vec<u8>>);
impl SinkPort for CollectSink {
    fn try_send(
        &mut self,
        _: DeliveryClass,
        bytes: &[u8],
    ) -> std::result::Result<(), DeliveryError> {
        self.0.push(bytes.to_vec());
        Ok(())
    }
}

fn id(prefix: &str, n: i64) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}

fn clip(n: i64, start: u64, frames: u64) -> StudioClip {
    StudioClip {
        schema_id: ClipSchema::V1,
        clip_id: id("SCLP", n),
        source_ref: ClipSource::Media {
            artifact_manifest_id: "manifest-a".into(),
            content_digest: ContentDigest {
                algorithm: "sha256".into(),
                digest: "abc123".into(),
            },
        },
        source_in: Ticks::new(0),
        source_out: Ticks::new(frames * F),
        timeline_start: Ticks::new(start * F),
        duration: Ticks::new(frames * F),
        speed: Speed::UNIT,
        enabled: true,
        link_group: None,
        markers: Vec::new(),
        transitions: hsk_studio_folio::ClipTransitions::default(),
    }
}

fn track(n: i64, kind: TrackKind, targeted: bool, clips: Vec<StudioClip>) -> StudioTrack {
    StudioTrack {
        schema_id: TrackSchema::V1,
        track_id: id("STRK", n),
        kind,
        index: 0,
        name: format!("track-{n}"),
        enabled: true,
        locked: false,
        sync_locked: true,
        targeted,
        muted: false,
        soloed: false,
        volume: 1.0,
        pan: 0.0,
        clips,
    }
}

fn fixture(cancel: &CancellationToken) -> Result<hsk_studio_folio::Snapshot> {
    let unassigned = || ProfileBinding::Unassigned(());
    let document = StudioDocument {
        schema_id: DocumentSchema::V1,
        document_id: id("SDOC", 1),
        revision: 1,
        geometry_unit: GeometryUnit::Px,
        colour_mode: 3,
        bits_per_channel: 8,
        working_space_rgb: unassigned(),
        working_space_cmyk: unassigned(),
        working_space_gray: unassigned(),
        working_space_spot: unassigned(),
        blending_space: unassigned(),
        artboards: vec![],
        page_spreads: vec![],
        layers: vec![],
        graph: StudioLayerGraph {
            schema_id: GraphSchema::V1,
            operations: vec![],
            edges: vec![],
            outputs: vec![],
        },
        sequences: vec![StudioSequence {
            schema_id: SequenceSchema::V1,
            sequence_id: "seq-main".into(),
            name: "main".into(),
            frame_rate: SequenceFrameRate { ticks_per_frame: F },
            duration: Ticks::new(11 * F),
            tracks: vec![
                track(
                    1,
                    TrackKind::Video,
                    true,
                    vec![clip(1, 0, 3), clip(2, 3, 3), clip(3, 8, 3)],
                ),
                track(
                    2,
                    TrackKind::Audio,
                    false,
                    vec![clip(4, 0, 2), clip(5, 8, 2)],
                ),
            ],
            markers: Vec::new(),
            work_area: None,
        }],
    };
    validate_document(document, Budget::default(), cancel, &Available)
        .map_err(|_| PulseError::FolioRejected)
}

fn arg<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).map(String::as_str)
}

fn num(args: &[String], name: &str, default: i64) -> Result<i64> {
    match arg(args, name) {
        None => Ok(default),
        Some(v) => v.parse().map_err(|_| PulseError::InvalidInput),
    }
}

fn frames_to_ticks_i(frames: i64) -> Result<Ticks> {
    u64::try_from(frames)
        .ok()
        .and_then(|f| f.checked_mul(F))
        .map(Ticks::new)
        .ok_or(PulseError::InvalidInput)
}

fn build_edit(args: &[String]) -> Result<TimelineEdit> {
    let clip_id = id("SCLP", num(args, "--clip", 1)?);
    let frames = num(args, "--frames", 2)?;
    let ripple = args.iter().any(|a| a == "--ripple");
    let delta = TickDelta::new(i128::from(frames) * i128::from(F))?;
    let fresh = match arg(args, "--new-id") {
        Some(_) => vec![id("SCLP", num(args, "--new-id", 0)?)],
        None => vec![],
    };
    let span = || -> Result<TickRange> {
        TickRange::new(
            frames_to_ticks_i(num(args, "--from", 1)?)?,
            frames_to_ticks_i(num(args, "--to", 2)?)?,
        )
    };
    Ok(match arg(args, "--op").unwrap_or("trim-out") {
        "trim-out" => TimelineEdit::TrimOut {
            clip_id,
            new_duration: frames_to_ticks_i(frames)?,
            ripple,
        },
        "trim-in" => TimelineEdit::TrimIn {
            clip_id,
            new_head: frames_to_ticks_i(frames)?,
            ripple,
        },
        "razor" => TimelineEdit::Razor {
            clip_id,
            at: frames_to_ticks_i(frames)?,
            new_right_clip_id: id("SCLP", num(args, "--new-id", 9)?),
            linked: None,
        },
        "roll" => TimelineEdit::Roll {
            left_clip_id: clip_id,
            delta,
        },
        "slip" => TimelineEdit::Slip { clip_id, delta },
        "slide" => TimelineEdit::Slide { clip_id, delta },
        "lift" => TimelineEdit::LiftClip { clip_id },
        "extract" => TimelineEdit::ExtractClip { clip_id },
        "lift-range" => TimelineEdit::LiftRange {
            span: span()?,
            fresh_clip_ids: fresh,
        },
        "extract-range" => TimelineEdit::ExtractRange {
            span: span()?,
            fresh_clip_ids: fresh,
        },
        "close-gap" => TimelineEdit::CloseGap {
            track_id: id("STRK", num(args, "--track", 1)?),
            at: frames_to_ticks_i(frames)?,
        },
        _ => return Err(PulseError::InvalidInput),
    })
}

fn window_json(w: ClipWindow) -> String {
    format!(
        "[{},{},{},{}]",
        w.source_in.value(),
        w.source_out.value(),
        w.timeline_start.value(),
        w.duration.value()
    )
}

fn list(ids: &[String]) -> String {
    ids.iter()
        .map(|i| format!("\"{i}\""))
        .collect::<Vec<_>>()
        .join(",")
}

fn change_json(c: &ClipChange) -> String {
    let (before, after) = (c.before(), c.after());
    let op = match (before, after) {
        (Some(_), Some(_)) => "retime",
        (None, Some(_)) => "insert",
        _ => "remove",
    };
    let side = |name: &str, w: Option<ClipWindow>| {
        w.map(|w| format!(",\"{name}\":{}", window_json(w)))
            .unwrap_or_default()
    };
    format!(
        "{{\"op\":\"{op}\",\"clip\":\"{}\",\"track\":\"{}\"{}{}}}",
        c.clip_id(),
        c.track_id(),
        side("before", before),
        side("after", after)
    )
}

fn plan(args: &[String], cancel: &CancellationToken) -> Result<String> {
    cancel.check()?;
    let snapshot = fixture(cancel)?;
    let edit = build_edit(args)?;
    let proposal = plan_timeline_edit(
        &snapshot,
        "seq-main",
        1,
        &edit,
        &EditContext {
            budget: Budget::default(),
            resolver: &Available,
            extents: &TenFrames,
            cancel,
        },
    )?;
    let changes: Vec<String> = proposal.changes().iter().map(change_json).collect();
    Ok(format!(
        "{{\"base_revision\":{},\"target_revision\":{},\"sequence_duration\":{},\"changes\":[{}],\"read\":[{}],\"write\":[{}]}}",
        proposal.base_revision(),
        proposal.target_revision(),
        proposal.snapshot().document().sequences[0].duration.value(),
        changes.join(","),
        list(proposal.read_footprint()),
        list(proposal.write_footprint())
    ))
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|a| a == "--descriptor" || a == "--help") {
        println!("{DESCRIPTOR}");
        return;
    }
    let cancel = CancellationToken::default();
    if args.iter().any(|a| a == "--cancel") {
        cancel.cancel();
    }
    let result = plan(&args, &cancel);
    let actor = ActorContext::new(
        "account",
        "principal",
        "owner",
        "owner-principal",
        "space",
        "session",
    )
    .expect("static context");
    let resource = DomainId::parse(&id("SDOC", 1)).expect("static id");
    let mut observe = Observe::new(
        1,
        1,
        resource,
        actor,
        ObserveBudget::new(0, 0).expect("budget"),
    );
    let mut sink = CollectSink(Vec::new());
    let receipt = report(&result, 1, 1, &cancel, &mut observe, &mut sink);
    match (result, receipt) {
        (Ok(json), Ok(r)) => {
            println!("{json}");
            eprintln!("outcome_code={} frames={}", r.outcome.wire(), sink.0.len());
        }
        (Err(e), _) => {
            eprintln!(
                "rejected:{}; consult --descriptor and correct only the caller input; no host proof",
                e.code()
            );
            std::process::exit(2)
        }
        (Ok(_), Err(e)) => {
            eprintln!("rejected:{}", e.code());
            std::process::exit(2)
        }
    }
}
