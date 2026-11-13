//! `prototype.*` (lane SMB-DRIVE-TYPED-2, STU-PRO-138/139): the in-editor player over a Figma `.fig`: the
//! spec's start_session, dispatch_trigger (a named trigger on a named node: no hit test, no pointer),
//! read_runtime_variables, capture_frame (an RGB PNG when a path is given, admitted through the viewer's
//! JobPort before the first byte, lane SMB-FIX-CAPTURE-ADMISSION-1; its size and channel means always)
//! and end_session, plus the play panel's frame-local click and hover, the virtual clock and
//! restart. Every reply carries the player's state and the events since the last row. Play mode changes
//! no document authority; one session per viewer (no session id yet).
#![deny(clippy::arithmetic_side_effects)]

use super::super::Viewer;
use super::super::develop::Refusal;
use super::super::prototype::{PlayFail, PrototypePlay, Screen};
use super::decode::{Fields, invalid};
use super::{CommandRefusal, read_bounded};
use hsk_studio_accord::DomainId;
use hsk_studio_components::PlayerInput;
use serde_json::{Value, json};

/// Longest clock step of one `prototype.advance` (one hour).
pub const MAX_ADVANCE_MS: u64 = 3_600_000;

fn refusal(e: PlayFail) -> CommandRefusal {
    CommandRefusal::new(e.code, json!({"message": e.detail}))
}

fn refused(r: Refusal) -> CommandRefusal {
    CommandRefusal::new(r.code, json!({"message": r.detail}))
}

fn state(play: &mut PrototypePlay) -> Value {
    let events: Vec<String> = play.take_events().iter().map(|e| format!("{e:?}")).collect();
    json!({"state": play.state_json(), "events": events})
}

/// Size and the mean of each channel: what the reply carries of a frame.
fn screen_json(screen: &Screen) -> Value {
    let mut sums = [0u64; 3];
    for px in screen.rgb.chunks_exact(3) {
        for (sum, c) in sums.iter_mut().zip(px) {
            *sum = sum.saturating_add(u64::from(*c));
        }
    }
    let pixels = u64::from(screen.width).saturating_mul(u64::from(screen.height)).max(1);
    json!({"width": screen.width, "height": screen.height, "mean_rgb": sums.map(|s| s.checked_div(pixels).unwrap_or(0))})
}

/// The job resource of a capture when no document is open (lead ruling 2026-11-12: never resource-less; as
/// `audio_import`'s audio file resource and SMB-WEB-SITE-1's site resource): an `SDOC` id in the UUIDv7 shape whose
/// digits are the SHA-256 of the play session's `.fig` source digest, so every capture of one prototype file
/// addresses one resource in every viewer and process. `None` when the session has no source digest (a decoded
/// tree; a viewer opens play sessions from bytes only) or the id does not parse.
fn prototype_resource(play: &PrototypePlay) -> Option<DomainId> {
    let digest = play.document().source_sha256?;
    let hex = hsk_studio_package::sha256_hex_of(&[b"hsk.studio.prototype.fig\0".as_slice(), digest.as_slice()].concat());
    let g = |r: std::ops::Range<usize>| hex.get(r).unwrap_or("0");
    let variant = match g(16..17) {
        "0" | "4" | "8" | "c" => "8",
        "1" | "5" | "9" | "d" => "9",
        "2" | "6" | "a" | "e" => "a",
        _ => "b",
    };
    DomainId::parse(&format!("SDOC-{}-{}-7{}-{variant}{}-{}", g(0..8), g(8..12), g(13..16), g(17..20), g(20..32))).ok()
}

/// `prototype.capture_frame` (module docs). With a path (lane SMB-FIX-CAPTURE-ADMISSION-1; R60 attempt 3 probe j2,
/// common lead ruling 2; STU-ARC-008, STU-ARC-013): the cheap checks first (`parameter_conflict`, a path with a file
/// name, `overwrite_required`), then one export job admitted through the viewer's bound JobPort before anything is
/// rendered or written (typed `overloaded`, `canceled`, `stale_revision`), then the render and the PNG write, settled
/// once (`completed`, else `canceled` or `failed` with the refusal's code); the reply carries the settlement as `job`.
/// The job names the open document, else the play session's resource ([`prototype_resource`]). Without a path
/// nothing is written and nothing is admitted: the frame is the reply's size and means. Test
/// `hsk-studio-harness` `tests/fix_capture_admission.rs`.
fn capture_frame(v: &mut Viewer, f: &Fields<'_>) -> Result<Value, CommandRefusal> {
    // `overwrite` applies to a written file only: refused without a path (review F7 on 2bbe2207).
    if f.has("overwrite") && !f.has("path") {
        return Err(CommandRefusal::new("parameter_conflict", json!({"keys": ["overwrite", "path"]})));
    }
    let closed = || CommandRefusal::new("prototype_closed", Value::Null);
    if !f.has("path") {
        let play = v.prototype().ok_or_else(closed)?;
        let screen = play.render().map_err(refusal)?;
        let mut r = state(play);
        r["screen"] = screen_json(&screen);
        return Ok(r);
    }
    let path = f.path("path")?;
    let overwrite = f.flag_or("overwrite", false)?;
    if path.file_name().is_none() {
        return Err(CommandRefusal::new("export_path", json!({"path": path.to_string_lossy()})));
    }
    if path.exists() && !overwrite {
        return Err(CommandRefusal::new("overwrite_required", json!({"path": path.to_string_lossy()})));
    }
    let admitted = if v.run.snapshot().is_some() {
        v.admit_export_job()
    } else {
        let play = v.prototype.as_ref().map(|p| &p.0).ok_or_else(closed)?;
        let resource = prototype_resource(play).ok_or_else(|| CommandRefusal::new("job_resource_invalid", json!({"message": "the play session has no source digest"})))?;
        v.admit_export_job_with(&v.job_admission_of(resource))
    };
    let job = admitted.map_err(refused)?;
    let result = match v.prototype() {
        Some(play) => render_and_write(play, &path),
        None => Err(Refusal { code: "prototype_closed", detail: String::new() }),
    };
    v.settle_export_job(job, result).map_err(refused)
}

/// The frame rendered and written as an 8-bit RGB PNG at `path` (the cheap checks passed, the job admitted); the
/// reply.
fn render_and_write(play: &mut PrototypePlay, path: &std::path::Path) -> Result<Value, Refusal> {
    let screen = play.render().map_err(|e| Refusal { code: e.code, detail: e.detail })?;
    let samples = hsk_studio_interop_raster::Samples::U8(screen.rgb.clone());
    let request = hsk_studio_interop_raster::PngEncodeRequest {
        width: screen.width,
        height: screen.height,
        colour: hsk_studio_interop_raster::Colour::Rgb,
        samples: &samples,
        icc: None,
        compression: hsk_studio_interop_raster::PngCompression::Balanced,
    };
    let png = hsk_studio_interop_raster::encode_png(&request).map_err(|e| Refusal { code: e.code(), detail: "png".to_string() })?;
    super::super::export::write_atomic(path, &png).map_err(|code| Refusal { code, detail: path.to_string_lossy().into_owned() })?;
    let mut r = state(play);
    r["screen"] = screen_json(&screen);
    Ok(r)
}

fn trigger(f: &Fields<'_>) -> Result<PlayerInput, CommandRefusal> {
    let node = f.text("node")?;
    Ok(match f.text("trigger")?.as_str() {
        "click" => PlayerInput::Click(node),
        "hover_enter" => PlayerInput::HoverEnter(node),
        "hover_leave" => PlayerInput::HoverLeave(node),
        "mouse_down" => PlayerInput::MouseDown(node),
        "mouse_up" => PlayerInput::MouseUp(node),
        "press" => PlayerInput::Press(node),
        _ => return Err(invalid("trigger", "click|hover_enter|hover_leave|mouse_down|mouse_up|press")),
    })
}

/// One `prototype.*` row (module docs).
pub(super) fn run(v: &mut Viewer, id: &str, f: Fields<'_>) -> Result<Value, CommandRefusal> {
    if id == "prototype.start_session" {
        let bytes = read_bounded(&f.path("path")?, crate::import::MAX_SOURCE_BYTES)?;
        v.open_prototype(&bytes).map_err(refusal)?;
    }
    if v.prototype.is_none() {
        return Err(CommandRefusal::new("prototype_closed", json!({"hint": "prototype.start_session first"})));
    }
    if id == "prototype.end_session" {
        v.prototype = None;
        return Ok(json!({"open": false}));
    }
    if id == "prototype.capture_frame" {
        return capture_frame(v, &f).map(|mut reply| {
            reply["open"] = json!(true);
            reply
        });
    }
    let play = v.prototype().ok_or_else(|| CommandRefusal::new("prototype_closed", Value::Null))?;
    let mut reply = match id {
        "prototype.start_session" | "prototype.state" => state(play),
        "prototype.dispatch_trigger" => {
            let input = trigger(&f)?;
            play.dispatch_trigger(&input);
            state(play)
        }
        "prototype.click" => {
            let hit = play.click(f.number("x")?, f.number("y")?);
            let mut r = state(play);
            r["hit"] = json!(hit);
            r
        }
        "prototype.hover" => {
            play.hover(f.number("x")?, f.number("y")?);
            state(play)
        }
        "prototype.advance" => {
            let ms = f.unsigned("ms")?;
            if ms > MAX_ADVANCE_MS {
                return Err(invalid("ms", "0..=3600000"));
            }
            play.advance(ms);
            state(play)
        }
        "prototype.restart" => {
            play.restart();
            state(play)
        }
        "prototype.read_runtime_variables" => {
            let s = play.session().state();
            let mut r = state(play);
            r["variables"] = serde_json::to_value(&s.variables).unwrap_or(Value::Null);
            r["variants"] = json!(s.variants);
            r
        }
        other => return Err(CommandRefusal::new("command_not_driveable", json!({"command_id": other}))),
    };
    reply["open"] = json!(true);
    Ok(reply)
}

/// Review of b9074096 (lead rulings 2026-11-13): F3 the derived resource follows the `.fig` digest and a session
/// without one is refused `job_resource_invalid`; F4 a canceled viewer token refuses the capture at admission, and a
/// cancel raised after admission settles the job `canceled` before the write.
#[cfg(test)]
mod tests {
    use super::super::super::Viewer;
    use super::super::super::prototype::PrototypePlay;
    use super::prototype_resource;
    use hsk_studio_accord::CancellationToken;
    use hsk_studio_session::{Cost, JobPort, JobRefused, JobScope, JobTicket};
    use serde_json::{Value, json};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Admits every ask and counts asks; with `cancel_on_admit` cancels that token inside the ask (a cancel raised
    /// after admission, before the render and the write).
    #[derive(Default)]
    struct Port {
        asks: AtomicU64,
        cancel_on_admit: Option<CancellationToken>,
    }

    impl JobPort for Port {
        fn admit(&self, _scope: &JobScope<'_>, _cost: Cost) -> Result<JobTicket, JobRefused> {
            self.asks.fetch_add(1, Ordering::SeqCst);
            if let Some(token) = &self.cancel_on_admit {
                token.cancel();
            }
            Ok(JobTicket(1))
        }
        fn release(&self, _ticket: JobTicket) {}
    }

    fn viewer() -> Viewer {
        use hsk_studio_accord::{ActorContext, ClientSessionId, TransientCorrelation};
        let actor = ActorContext::new("prototype-unit", "principal", "owner", "owner-principal", "space", "session").unwrap();
        let context = hsk_studio_harness_core::RunContext::new(actor, ClientSessionId::parse("prototype-unit").unwrap(), Arc::new(TransientCorrelation::new()));
        Viewer::with_context(super::super::super::render::Prefer::Cpu, hsk_studio_render_gpu::DeviceOptions::default(), context)
    }

    /// The Operator prototype fixture (repository-relative, never machine-local).
    fn fig() -> Vec<u8> {
        std::fs::read(format!("{}/../hsk-studio-interop-fig/tests/fixtures/fig-prototype-pages.fig", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    /// A play session over the fixture's decoded document with `digest` as its source digest.
    fn session_with(play: &PrototypePlay, digest: Option<[u8; 32]>) -> PrototypePlay {
        let mut doc = play.document().clone();
        doc.source_sha256 = digest;
        PrototypePlay::open_document(doc, None).unwrap()
    }

    /// A scratch folder of its own (process id and test name).
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hsk-prototype-unit-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn capture(v: &mut Viewer, path: &std::path::Path) -> Result<Value, super::CommandRefusal> {
        let Value::Object(params) = json!({"path": path.to_str().unwrap()}) else { unreachable!() };
        v.run_command("prototype.capture_frame", &params)
    }

    fn start(v: &mut Viewer, port: &Arc<Port>) {
        v.set_job_port(port.clone());
        v.open_prototype(&fig()).unwrap();
    }

    fn names(dir: &std::path::Path) -> Vec<String> {
        std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect()
    }

    /// Review F3: two different source digests give two different `SDOC` ids, each the same every time; the
    /// fixture's own session derives from its own digest; no digest derives nothing.
    #[test]
    fn the_derived_resource_follows_the_fig_digest() {
        let play = PrototypePlay::open(&fig(), None).unwrap();
        let a = prototype_resource(&session_with(&play, Some([1; 32]))).unwrap();
        let b = prototype_resource(&session_with(&play, Some([2; 32]))).unwrap();
        assert_ne!(a, b, "distinct digests, distinct resources");
        assert!(a.as_str().starts_with("SDOC-") && b.as_str().starts_with("SDOC-"), "{a:?} {b:?}");
        assert_eq!(prototype_resource(&session_with(&play, Some([1; 32]))), Some(a.clone()), "stable");
        let own = prototype_resource(&play).unwrap();
        assert_eq!(prototype_resource(&session_with(&play, play.document().source_sha256)), Some(own.clone()));
        assert!(own != a && own != b);
        assert_eq!(prototype_resource(&session_with(&play, None)), None);
    }

    /// Review F3: with no document open, a play session without a source digest is refused `job_resource_invalid`
    /// before the port is asked (never resource-less, lead ruling 2026-11-12); nothing is written.
    #[test]
    fn a_session_without_a_source_digest_is_refused_job_resource_invalid() {
        let dir = scratch("no-digest");
        let mut v = viewer();
        let port = Arc::new(Port::default());
        v.set_job_port(port.clone());
        let play = PrototypePlay::open(&fig(), None).unwrap();
        v.prototype = Some((session_with(&play, None), std::time::Instant::now()));
        let path = dir.join("frame.png");
        let e = capture(&mut v, &path).unwrap_err();
        assert_eq!(e.code, "job_resource_invalid", "{e:?}");
        assert_eq!(port.asks.load(Ordering::SeqCst), 0);
        assert!(names(&dir).is_empty(), "nothing written");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Review F4 (a): a canceled viewer token refuses the capture at admission, typed `canceled`, before the port
    /// is asked; nothing is written.
    #[test]
    fn a_canceled_token_refuses_the_capture_at_admission() {
        let dir = scratch("canceled");
        let mut v = viewer();
        let port = Arc::new(Port::default());
        start(&mut v, &port);
        v.cancel.cancel();
        let path = dir.join("frame.png");
        let e = capture(&mut v, &path).unwrap_err();
        assert_eq!(e.code, "canceled", "{e:?}");
        assert_eq!(port.asks.load(Ordering::SeqCst), 0);
        let counts = v.job_counts();
        assert_eq!((counts.refused_canceled, counts.admitted), (1, 0), "{counts:?}");
        assert!(names(&dir).is_empty(), "nothing written");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Review F4 (b): a cancel raised after admission (inside the port's ask) is checked after the render and
    /// before the write: refused `canceled`, the job settled once `canceled` (not `failed`, not `completed`),
    /// nothing written. A cancel during the render itself is not observed: `PrototypePlay` renders under its
    /// own token (not_proven, lane notes).
    #[test]
    fn a_cancel_after_admission_settles_canceled_before_the_write() {
        let dir = scratch("canceled-after");
        let mut v = viewer();
        let port = Arc::new(Port { cancel_on_admit: Some(v.cancel.clone()), ..Port::default() });
        start(&mut v, &port);
        let path = dir.join("frame.png");
        let e = capture(&mut v, &path).unwrap_err();
        assert_eq!(e.code, "canceled", "{e:?}");
        assert_eq!(port.asks.load(Ordering::SeqCst), 1);
        let counts = v.job_counts();
        assert_eq!((counts.admitted, counts.canceled, counts.failed, counts.completed, counts.abandoned, counts.released, counts.in_flight), (1, 1, 0, 0, 0, 1, 0), "{counts:?}");
        assert!(names(&dir).is_empty(), "nothing written, no temp");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
