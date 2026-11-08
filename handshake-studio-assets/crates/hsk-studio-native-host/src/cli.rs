//! `studio-vk` command line. Anything unknown is a usage error (exit 2) and never opens a window.
use crate::error::HostError;
use crate::offscreen::OffscreenArgs;
use crate::scene::{ChromeMode, DEFAULT_CLEAR, GuiArgs, Scene};
use crate::script::{self, Step};
use crate::window::{MAX_FRAMES, WindowArgs};
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
  studio-vk help
Offscreen opens no window. window --quiet never takes focus and needs --frames. Scene gui: the document in the
Viewport beside the Layers panel (default size 1280x800). Exit: 0 ok, 2 usage, 3 Vulkan refusal, 4 io, 5 focus
violation, 6 timeout, 7 document, 8 window. Receipts name the validation state (on in debug builds when the loader
lists VK_LAYER_KHRONOS_validation, absent when not, off in release builds) and its error and warning counts.";

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
