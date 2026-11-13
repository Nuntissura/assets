//! `studio-vk` command line. Anything unknown is a usage error (exit 2) and never opens a window. [`caller`] reads who
//! runs it and [`observe`] where its terminal frame goes, apart from [`parse`], so a refused command line still has
//! both (F-R60-22).
use crate::error::HostError;
use crate::offscreen::OffscreenArgs;
use crate::scene::{ChromeMode, DEFAULT_CLEAR, GuiArgs, Scene};
use crate::script::{self, Step};
use crate::window::{MAX_FRAMES, WindowArgs};
use hsk_studio_accord::{ActorContext, DomainId};
use hsk_studio_vulkan::rgba_len;
use std::path::PathBuf;
use std::time::Duration;

pub const USAGE: &str = "studio-vk: asset-local Vulkan host (chrome: cpu painter by default, vulkan ui pass with --chrome vulkan)
  studio-vk offscreen [--size WxH] [--scene clear|painted|document|gui] [--clear R,G,B,A] [--document PATH]
                      [--out X.png] [--receipt X.json] [--allow-overlays]
                      [--script S.txt] [--invoke AUTHOR_ID]... [--tree T.json] [--scale 1.25]   (scene gui)
                      [--chrome cpu|vulkan] [--ui-trace T.json]                                    (scene gui)
  studio-vk window [--quiet] [--frames N] [--exercise] [--size WxH] [--scene ...] [--clear R,G,B,A]
                   [--document PATH] [--capture Y.png] [--receipt Y.json] [--deadline-s S] [--allow-overlays]
                   [--script S.txt] [--invoke AUTHOR_ID]... [--tree T.json] [--scale 1.25]        (scene gui)
                   [--chrome cpu|vulkan] [--ui-trace T.json]                                         (scene gui)
  offscreen and window: [--actor ID --session ID --resource SDOC-...] [--observe O.jsonl]
  studio-vk help
Offscreen opens no window. window --quiet never takes focus and needs --frames. Scene gui: the document in the
Viewport beside the Layers panel (default size 1280x800). Exit: 0 ok, 2 usage, 3 Vulkan refusal, 4 io, 5 focus
violation, 6 timeout, 7 document, 8 window. Receipts name the validation state (on in debug builds when the loader
lists VK_LAYER_KHRONOS_validation, absent when not, off in release builds) and its error and warning counts.
Every run but help sends one terminal observe frame, refusals included, as one JSON line to --observe (appended) or
stderr: as the caller --actor/--session/--resource name (all three or none), else as the asset-local test host's
fixed context (caller_default true).";

/// The resource of a run whose command line names no caller: a fixed id beside the asset-local test host's fixed
/// actor ([`Caller::asset_local`]).
pub const ASSET_LOCAL_RESOURCE: &str = "SDOC-00000000-0000-7000-8000-000000000000";

/// Who runs one studio-vk run (HOST INTEGRATION (4)), as the harness CLI reads it: `--actor` is the account and the
/// principal, owner and access space default to it (a stated limit: nothing here proves a grant), `--session` is the
/// session, `--resource` the resource id. A command line with none of the three runs as [`Caller::asset_local`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caller {
    pub actor: ActorContext,
    pub resource: DomainId,
    /// The command line named no caller: the asset-local test host's fixed context.
    pub caller_default: bool,
}

impl Caller {
    /// The asset-local test host's fixed context: the actor harness-view's `Viewer::new` runs on (harness-core
    /// `asset_local_test_context`; tests/fix_host_terminal_frame.rs pins the two equal under feature `document`) and
    /// [`ASSET_LOCAL_RESOURCE`].
    pub fn asset_local() -> Self {
        Self {
            actor: ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session")
                .expect("static actor literals satisfy accord bounds"),
            resource: DomainId::parse(ASSET_LOCAL_RESOURCE).expect("static resource id satisfies accord"),
            caller_default: true,
        }
    }
}

/// Flags that take a value; every other flag stands alone. [`run_flags`] steps over values exactly as [`parse`] does.
const VALUE_FLAGS: [&str; 19] = [
    "--size", "--scene", "--clear", "--document", "--out", "--capture", "--receipt", "--frames", "--deadline-s", "--script",
    "--invoke", "--tree", "--scale", "--chrome", "--ui-trace", "--actor", "--session", "--resource", "--observe",
];

#[derive(Default)]
struct RunFlags<'a> {
    actor: Option<&'a str>,
    session: Option<&'a str>,
    resource: Option<&'a str>,
    observe: Option<&'a str>,
    /// The first refusal: a caller or observe flag given twice or without a value.
    refusal: Option<HostError>,
}

/// The caller and observe flags after the command word, read whatever else the command line holds.
fn run_flags<S: AsRef<str>>(args: &[S]) -> RunFlags<'_> {
    let mut flags = RunFlags::default();
    let mut it = args.iter().map(AsRef::as_ref).skip(1);
    while let Some(flag) = it.next() {
        let slot = match flag {
            "--actor" => &mut flags.actor,
            "--session" => &mut flags.session,
            "--resource" => &mut flags.resource,
            "--observe" => &mut flags.observe,
            f if VALUE_FLAGS.contains(&f) => {
                it.next();
                continue;
            }
            _ => continue,
        };
        let refusal = match it.next() {
            None => Some(HostError::Usage("missing_value")),
            Some(value) => slot.replace(value).map(|_| HostError::Usage("flag_repeated")),
        };
        flags.refusal = flags.refusal.or(refusal);
    }
    flags
}

/// The caller of a run: all three caller flags, or none of them ([`Caller::asset_local`]). Anything else is a usage
/// refusal: `caller_incomplete`, `actor_invalid` (accord bounds: 1..=128 bytes, no control character),
/// `resource_invalid` (an accord domain id), `flag_repeated`, `missing_value`.
pub fn caller<S: AsRef<str>>(args: &[S]) -> Result<Caller, HostError> {
    let flags = run_flags(args);
    if let Some(refusal) = flags.refusal {
        return Err(refusal);
    }
    match (flags.actor, flags.session, flags.resource) {
        (None, None, None) => Ok(Caller::asset_local()),
        (Some(actor), Some(session), Some(resource)) => Ok(Caller {
            actor: ActorContext::new(actor, actor, actor, actor, actor, session).map_err(|_| HostError::Usage("actor_invalid"))?,
            resource: DomainId::parse(resource).map_err(|_| HostError::Usage("resource_invalid"))?,
            caller_default: false,
        }),
        _ => Err(HostError::Usage("caller_incomplete")),
    }
}

/// The caller's `--observe` file, when the command line names one (a refused command line included).
pub fn observe<S: AsRef<str>>(args: &[S]) -> Option<PathBuf> {
    run_flags(args).observe.map(PathBuf::from)
}

/// GUI scene size when no `--size` is given.
pub const GUI_DEFAULT_SIZE: (u32, u32) = (1280, 800);
/// Wall-clock bound of the Operator's own window (`window` without `--quiet` and without `--frames`) when no
/// `--deadline-s` is given: it waits for events and must not close after the 30 s run default.
pub const OPERATOR_DEADLINE: Duration = Duration::from_secs(12 * 3600);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Offscreen(OffscreenArgs),
    Window(WindowArgs),
}

fn size(v: &str) -> Result<(u32, u32), HostError> {
    let (w, h) = v.split_once(['x', 'X']).ok_or(HostError::Usage("size"))?;
    let w: u32 = w.parse().map_err(|_| HostError::Usage("size"))?;
    let h: u32 = h.parse().map_err(|_| HostError::Usage("size"))?;
    rgba_len(w, h).map_err(|_| HostError::Usage("size"))?;
    Ok((w, h))
}

fn colour(v: &str) -> Result<[u8; 4], HostError> {
    let parts: Vec<&str> = v.split(',').collect();
    if parts.len() != 4 {
        return Err(HostError::Usage("clear"));
    }
    let mut out = [0u8; 4];
    for (o, p) in out.iter_mut().zip(parts) {
        *o = p.trim().parse().map_err(|_| HostError::Usage("clear"))?;
    }
    Ok(out)
}

/// A UI scale given as a decimal (`1`, `1.25`): thousandths in 500..=8000 (kumihimo-ui `Scale::new`).
fn scale(v: &str) -> Result<u32, HostError> {
    let s: f64 = v.parse().map_err(|_| HostError::Usage("scale"))?;
    if !s.is_finite() || !(0.5..=8.0).contains(&s) {
        return Err(HostError::Usage("scale"));
    }
    Ok((s * 1000.0).round() as u32)
}

/// Parses `args` (without the program name).
pub fn parse<S: AsRef<str>>(args: &[S]) -> Result<Command, HostError> {
    let mut it = args.iter().map(AsRef::as_ref);
    let mode = match it.next() {
        Some("help" | "--help" | "-h") => return Ok(Command::Help),
        Some(m @ ("offscreen" | "window")) => m,
        Some(_) => return Err(HostError::Usage("unknown_command")),
        None => return Err(HostError::Usage("no_command")),
    };
    let mut wh: Option<(u32, u32)> = None;
    let mut scene_name: Option<String> = None;
    let mut clear = DEFAULT_CLEAR;
    let mut document: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut receipt: Option<PathBuf> = None;
    let mut quiet = false;
    let mut exercise = false;
    let mut frames: Option<u32> = None;
    let mut deadline: Option<Duration> = None;
    let mut allow_overlays = false;
    let mut gui = GuiArgs::default();
    let mut gui_flag = false;
    let window = mode == "window";
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or(HostError::Usage("missing_value"));
        match flag {
            "--size" => wh = Some(size(value()?)?),
            "--scene" => scene_name = Some(value()?.to_string()),
            "--clear" => clear = colour(value()?)?,
            "--document" => document = Some(PathBuf::from(value()?)),
            "--out" if !window => out = Some(PathBuf::from(value()?)),
            "--capture" if window => out = Some(PathBuf::from(value()?)),
            "--receipt" => receipt = Some(PathBuf::from(value()?)),
            "--quiet" if window => quiet = true,
            "--exercise" if window => exercise = true,
            "--frames" if window => {
                let n: u32 = value()?.parse().map_err(|_| HostError::Usage("frames"))?;
                if n > MAX_FRAMES {
                    return Err(HostError::Usage("frames"));
                }
                frames = Some(n);
            }
            "--deadline-s" if window => {
                let s: u64 = value()?.parse().map_err(|_| HostError::Usage("deadline"))?;
                if s == 0 || s > 3600 {
                    return Err(HostError::Usage("deadline"));
                }
                deadline = Some(Duration::from_secs(s));
            }
            "--allow-overlays" => allow_overlays = true,
            // Read by `caller` and `observe`.
            "--actor" | "--session" | "--resource" | "--observe" => {
                value()?;
            }
            "--script" => {
                let steps = script::read(std::path::Path::new(value()?))?;
                script::extend(&mut gui.steps, steps)?;
                gui_flag = true;
            }
            "--invoke" => {
                let id = value()?.to_string();
                script::extend(&mut gui.steps, [Step::Invoke(id)])?;
                gui_flag = true;
            }
            "--tree" => {
                gui.tree = Some(PathBuf::from(value()?));
                gui_flag = true;
            }
            "--scale" => {
                gui.scale_milli = Some(scale(value()?)?);
                gui_flag = true;
            }
            "--chrome" => {
                gui.chrome = match value()? {
                    "cpu" => ChromeMode::Cpu,
                    "vulkan" => ChromeMode::Vulkan,
                    _ => return Err(HostError::Usage("chrome")),
                };
                gui_flag = true;
            }
            "--ui-trace" => {
                gui.ui_trace = Some(PathBuf::from(value()?));
                gui_flag = true;
            }
            _ => return Err(HostError::Usage("unknown_flag")),
        }
    }
    let scene = match (scene_name.as_deref(), document) {
        (Some("gui"), Some(path)) => Scene::Gui(path),
        (Some("gui"), None) => return Err(HostError::Usage("gui_needs_document")),
        (None | Some("document"), Some(path)) => Scene::Document(path),
        (Some("document"), None) => return Err(HostError::Usage("document_needs_path")),
        (None | Some("clear"), None) => Scene::Clear(clear),
        (Some("painted"), None) => Scene::Painted,
        _ => return Err(HostError::Usage("scene")),
    };
    let is_gui = matches!(scene, Scene::Gui(_));
    if gui_flag && !is_gui {
        return Err(HostError::Usage("gui_only"));
    }
    let (w, h) = wh.unwrap_or(if is_gui { GUI_DEFAULT_SIZE } else { (256, 192) });
    if !window {
        return Ok(Command::Offscreen(OffscreenArgs { width: w, height: h, scene, out, receipt, allow_overlays, gui }));
    }
    let frames = match (quiet, frames) {
        (true, None) => return Err(HostError::Usage("quiet_needs_frames")),
        (true, Some(0)) => return Err(HostError::Usage("quiet_needs_frames")),
        (_, Some(n)) => n,
        (false, None) => 0,
    };
    if exercise && !quiet {
        return Err(HostError::Usage("exercise_needs_quiet"));
    }
    let deadline = deadline.unwrap_or(if frames == 0 { OPERATOR_DEADLINE } else { Duration::from_secs(30) });
    Ok(Command::Window(WindowArgs {
        width: w,
        height: h,
        scene,
        quiet,
        frames,
        exercise,
        capture: out,
        receipt,
        allow_overlays,
        deadline,
        gui,
    }))
}
