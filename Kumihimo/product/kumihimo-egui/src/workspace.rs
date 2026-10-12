//! Ephemeral top-down geometry; the Controller remains the only placement owner.
use crate::*;
use kumihimo_core::{GeometryContext, ToolPresentation};
use std::collections::{BTreeSet, VecDeque};

#[derive(Clone, Debug)]
pub struct FrameProjection {
    pub frame: FrameId,
    pub workspace: WorkspaceId,
    pub bounds: Rect,
    pub content: Rect,
    pub usable: Rect,
    pub clip: Rect,
    pub depth: usize,
    pub floating: bool,
    pub child: Option<WorkspaceId>,
    pub z_root: Option<FrameId>,
    pub settings: AdapterSettings,
    pub settings_source: &'static str,
    pub recreation_needed: bool,
}
impl FrameProjection {
    pub fn layer(&self) -> egui::LayerId {
        egui::LayerId::new(
            if self.z_root.is_some() {
                egui::Order::Foreground
            } else {
                egui::Order::Middle
            },
            egui::Id::new((
                "kumihimo-surface",
                self.z_root.map(|f| f.raw()).unwrap_or(0),
            )),
        )
    }
}
#[derive(Clone, Debug)]
pub struct ToolProjection {
    pub tool: ToolId,
    pub bounds: Rect,
    pub clip: Rect,
    pub presentation: ToolPresentation,
}
#[derive(Clone, Debug, Default)]
pub struct Projection {
    pub generation: u64,
    pub frames: Vec<FrameProjection>,
    pub tools: Vec<ToolProjection>,
    pub rejected: Vec<String>,
}
pub fn project(
    controller: &mut Controller,
    surface: SurfaceBinding,
    host: &mut impl DesktopHost,
) -> Result<Projection> {
    project_with_chrome(controller, surface, host, &chrome::ChromeState::default())
}
pub fn project_with_chrome(
    controller: &mut Controller,
    surface: SurfaceBinding,
    host: &mut impl DesktopHost,
    chrome: &chrome::ChromeState,
) -> Result<Projection> {
    surface.validate()?;
    let policy = host.policy();
    if policy.max_regions == 0 || policy.tab_page_size == 0 || policy.max_pending == 0 {
        return Err(AdapterError::Invalid("zero host budget"));
    }
    if controller
        .snapshot()
        .workspaces
        .get(&surface.root)
        .is_none_or(|w| w.owner.is_some())
    {
        return Err(AdapterError::Invalid(
            "surface must reference canonical root",
        ));
    }
    let mut output = Projection::default();
    let mut pending =
        VecDeque::from([(surface.root, surface.bounds, surface.bounds, 0usize, None)]);
    let mut seen = BTreeSet::new();
    while let Some((workspace, bounds, ancestor_clip, depth, inherited_z)) = pending.pop_front() {
        let (settings, settings_source, recreation_needed) =
            chrome.resolve(&policy, workspace, surface.viewport)?;
        if !seen.insert(workspace) {
            return Err(AdapterError::Invalid("workspace containment cycle"));
        }
        let mut context = controller
            .snapshot()
            .contexts
            .get(&workspace)
            .cloned()
            .unwrap_or_else(|| GeometryContext::new(bounds));
        context.viewport = bounds;
        if controller.snapshot().contexts.get(&workspace) != Some(&context) {
            let response = host.dispatch(
                controller,
                Command::SetGeometryContext { workspace, context },
            )?;
            response.outcome.result?;
        }
        let geometry = match controller.snapshot().geometry(workspace) {
            Ok(g) => g,
            Err(e) => {
                output.rejected.push(format!(
                    "workspace {}: {e}; use pane list or resize",
                    workspace.raw()
                ));
                continue;
            }
        };
        for (&tool, &rect) in &geometry.tools {
            if output.frames.len() + output.tools.len() >= policy.max_regions {
                output
                    .rejected
                    .push("projection budget; canonical pane list remains available".into());
                break;
            }
            if let Some(state) = geometry.tool_states.get(&tool) {
                output.tools.push(ToolProjection {
                    tool,
                    bounds: rect,
                    clip: intersect(rect, ancestor_clip)?,
                    presentation: state.clone(),
                });
            }
        }
        // Core omits maximized siblings from geometry; never traverse their children.
        let ids = controller.snapshot().workspaces[&workspace].frames.clone();
        for frame in ids {
            let Some(&rect) = geometry.frames.get(&frame) else {
                continue;
            };
            if output.frames.len() + output.tools.len() >= policy.max_regions {
                output
                    .rejected
                    .push("projection budget; canonical pane list remains available".into());
                break;
            }
            let content = geometry.frame_content.get(&frame).copied().unwrap_or(rect);
            let usable = usable_content(content, &settings)?;
            let clip = intersect(
                geometry.clips.get(&frame).copied().unwrap_or(rect),
                ancestor_clip,
            )?;
            let record = &controller.snapshot().frames[&frame];
            let child = record.child_workspace;
            let floating = controller.snapshot().floats.contains_key(&frame);
            let z_root = if floating { Some(frame) } else { inherited_z };
            output.frames.push(FrameProjection {
                frame,
                workspace,
                bounds: rect,
                content,
                usable,
                clip,
                depth,
                floating,
                child,
                z_root,
                settings: settings.clone(),
                settings_source,
                recreation_needed,
            });
            if let Some(child) = child {
                let child_clip = intersect(usable, clip)?;
                if child_clip.width() > 0.0 && child_clip.height() > 0.0 {
                    pending.push_back((
                        child,
                        usable,
                        child_clip,
                        depth
                            .checked_add(1)
                            .ok_or(AdapterError::Budget("depth arithmetic"))?,
                        z_root,
                    ));
                }
            }
        }
    }
    // Owner first, then host-ordered floats; stable ordering is derived, never canonical.
    output.frames.sort_by_key(|f| {
        (
            f.z_root.is_some(),
            f.z_root
                .and_then(|id| policy.float_order.iter().position(|v| *v == id))
                .unwrap_or(0),
            f.z_root.map(|f| f.raw()).unwrap_or(0),
            f.depth,
        )
    });
    output.generation = controller.snapshot().generation;
    Ok(output)
}
