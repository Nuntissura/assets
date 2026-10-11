---
file_id: RESEARCH-KUM-SURFACE-RIFFS-v1
file_kind: exploratory-preparation
updated_at: '2026-10-11'
---

<topic id="intent-and-scope" status="proposed" wp="WP-KUM-003-surface-riffs-v1">

# Overlapping frames, animated borders and a temporary minimap

Prep only. This note preserves ideas and evaluates them; it does not adopt product requirements or select an implementation. The current specification and product files remain unchanged.

Operator concepts:

- A child window can sit in the center over another window, while the uncovered parts of the underlying content remain usable. Other children can be placed beside the center child.
- Child snapping uses a grid calculated from the parent's size. Parent geometry relates to the app or screen size. Subdivision counts are adjustable, with settings exposed to the host.
- Explore morphing borders resembling liquid, sand, fire or smoke, including their effects on inner content and neighboring host content.
- Grabbing the scrollbar reveals a minimap in place of the rail/thumb. Continue the same drag using the viewport rectangle over the miniature document; the map expands beside the main document. Release hides the map and restores the ordinary rail/thumb. The operator explicitly confirmed this interpretation.
- Evaluate and iterate before deciding what to keep or discard; an idea's presence here creates no implementation commitment.

</topic>

<topic id="parent-relative-snapping" status="proposed">

## A snapping grid is separate from tiling

Proposed interpretation: each parent can provide a tiled layer and an overlapping floating layer. Both may use its snapping guides. A snap target aligns geometry; it need not reserve an exclusive cell or force neighboring content to move.

This makes the central-child idea coherent: a floating preview, reference image, mini-player or tool card can sit above a large editor, document, diagram or scene. Neighboring floating cards can snap alongside it. These are hypothetical use cases, not findings about an implemented Kumihimo feature.

The uncovered background can remain interactive. The covered pixels cannot simultaneously expose the foreground and hidden controls to an ordinary click. Candidates for temporary access are moving/minimizing the child, a reveal-underlay command, or explicit click-through mode. Click-through must be visibly indicated; transparency alone should not be treated as permission to click through.

Proposed geometry chain:

```text
screen work area -> application usable area -> parent usable area -> local snap grid -> child bounds
```

Use the parent's usable content rectangle, excluding its bars and reserved navigation areas. Illustrative example: a 1200 by 800 usable area with 12 columns and 8 rows gives 100 by 100 cells before gutters. Those numbers are an example, not defaults.

Host settings worth evaluating: rows/columns, gaps, edge or center snapping, threshold, grid origin, overlay visibility, overlap policy, containment, minimum child size and resize anchoring. Distinguish snapping guides from an exclusive tiled layout. Parent resizing needs a chosen rule: retain proportional cell occupancy, retain pixel size and re-snap, or preserve an anchor. Combining these implicitly would cause surprising motion.

Potential trap: clicking the background raises its entire frame over the center child. Candidate mitigation: keep parent content focus separate from child stacking order, with explicit bring-forward actions. A non-modal child is appropriate to the desired uncovered-background interaction; a modal dialog deliberately blocks it.

Precedent: [PowerToys FancyZones](https://learn.microsoft.com/en-us/windows/powertoys/fancyzones) supplies customizable zones and behavior for overlapping zones. It is a desktop-window precedent, not evidence of this exact nested parent-relative interaction.

[Qt QMdiArea](https://doc.qt.io/qt-6/qmdiarea.html) provides in-app subwindows with stacking, cascading and tiling. [Dockview accessibility](https://dockview.dev/docs/advanced/accessibility/) describes floating groups as non-modal, leaving the grid behind reachable. These support separating focus/modal policy from placement.

[React Grid Layout](https://github.com/react-grid-layout/react-grid-layout) exposes container width, column count and overlapping items; its [calculation source](https://raw.githubusercontent.com/react-grid-layout/react-grid-layout/master/src/core/calculate.ts) derives horizontal cells from available width. Vertical rows use a configured pixel height. [Interact.js snapping](https://interactjs.io/docs/snapping/) supports parent-relative origins and grid targets; [restriction](https://interactjs.io/docs/restriction/) can confine movement to a parent. Adjustable subdivision of both parent dimensions is a Kumihimo adaptation, not an exact claim about these projects.

Provisional evaluation: retain adjustable parent-relative snapping for further planning. Keep overlapping children as an optional presentation mode alongside ordinary tiling; useful examples exist as design scenarios, but the preferred resize/focus behavior remains open.

Future paper-review or focused prototype scenarios: shrink the parent; change its grid count; move a center child; click an uncovered parent control; focus by keyboard; access a hidden parent control; add a neighboring child; restore a saved overlapping layout.

</topic>

<topic id="morphing-border-levels" status="proposed">

## Morphing borders have four very different meanings

| Level | What changes | Inner content | Surrounding content |
| --- | --- | --- | --- |
| Decorative border | Color, particles or shader motion around stable geometry | Layout, text and input rectangle stay stable | May cover neighboring pixels if its visual envelope is allowed to extend outward; decoration can ignore input |
| Changing outline | The visible silhouette changes through a shape or clip | Content can remain rectangular inside a fixed safe inset, but moving clipping can hide edge content | Visual shape and clickable shape need separate explicit rules |
| Deformed content surface | The rendered contents themselves bend, melt or ripple | Text/images distort; selection and pointer coordinates need matching transforms | Extra compositing can affect overlap and layering; difficult interactions need a settled-state fallback |
| Deformed layout | The changing shape also moves controls and pushes neighboring panes | Actual content bounds change, potentially causing host-owned text reflow | Neighbors may resize/reflow; input, focus and accessibility must follow the new arrangement |

Proposed daily-work preference: decorative border with a stable readable interior and stable resize handles. A moving visible edge does not have to move layout geometry. This is a design recommendation, not a benchmark or adopted requirement.

Existing examples support the concept: [Pixdecor](https://github.com/soreau/pixdecor) is a Wayfire decoration plugin with optional animated effects. [Burn My Windows](https://github.com/Schneegans/Burn-My-Windows) explores window transition effects including fire, melt, dust and wisps. These Linux compositor projects do not prove a Rust Windows/web adapter or permanent interactive liquid content.

Pixdecor's [configuration](https://github.com/soreau/pixdecor/blob/master/metadata/pixdecor.xml) includes smoke/ink and other effects. Its [decoration source](https://github.com/soreau/pixdecor/blob/master/src/deco-subsurface.cpp) separates pointer/touch move, resize and button regions from effect rendering. Its [effects source](https://github.com/soreau/pixdecor/blob/master/src/deco-effects.cpp) also shows that smoke/ink can require multiple compute passes and velocity/density textures. A border effect is not automatically cheap simply because its visible rim is narrow.

[KWin's wobbly-window implementation](https://github.com/KDE/kwin/blob/master/src/plugins/wobblywindows/wobblywindows.cpp) deforms a grid of window imagery. [Wayfire's fire implementation](https://github.com/WayfireWM/wayfire/blob/master/plugins/animate/fire/fire.cpp) uses particles and enlarges painting bounds beyond the window. These are concrete references for deformation and spill accounting, not proof that editable controls acquire matching transformed hit regions.

Cost dimensions for any later evaluation: animated pixel area, concurrent effects, pass count, particles, intermediate textures, texture copying and scheduling. No frame-time, CPU, GPU-memory or battery figures are established. Candidate controls include intensity, rim thickness, spill distance, trigger, duration, static fallback and host-controlled visibility/resource policy.

The [SVG interaction specification](https://www.w3.org/TR/SVG/interact.html) distinguishes visual masking from clipping and hit testing. Fully transparent masked pixels do not automatically let events through. This is a relevant web boundary precedent; native adapters need their own equivalent rules.

</topic>

<topic id="effect-use-cases" status="proposed">

## Riffing on liquid, sand, fire and smoke

The following are speculative uses, not selected features:

| Effect | Potential use | Concern and practical restraint |
| --- | --- | --- |
| Liquid | A soft join when a tab group docks; a ripple confirming an accepted drop; atmospheric media/gallery frames | Constant wobble could make edges feel unstable. Use short motion around a stable interior; stop after the action. |
| Sand | A border that drains during a known transfer or dissolves during a confirmed unload | A progress metaphor can imply facts the provider does not know. Use actual progress when available, otherwise an explicitly indeterminate animation with text status. |
| Fire | A brief active-task accent, an immersive game's frame theme or a close transition | Persistent flames compete with reading; red may imply an error. Keep meaning host-defined, add ordinary status labels and offer a quiet/static mode. |
| Smoke | A temporary transition into a confirmed dormant state, or an ambient themed panel | Smoke can obscure neighboring content and make controls unclear. Bound its painted area, fade it quickly and keep input/controls unaffected. |
| Woven motion | A subtle group border visually joining related frames | Motion may add little beyond a color label. Compare a static label before adopting continuous animation. |

Useful adjacent directions: let themes supply decorative renderers; expose intensity, paint envelope and animation policy; reduce motion while reading or selecting; pause decorative work when hidden. These reuse the already specified theme, geometry, visibility and resource-provider boundaries. Their usability and actual cost still require observation if ever implemented.

Refraction is a separate choice: if a liquid border samples and warps content behind it, it can visibly distort neighboring text without changing that text's own layout. It requires a supported composition path and additional rendering work whose cost is unmeasured here. A border that merely paints colors/particles does not need that effect.

Provisional evaluation: explore brief decorative effects and optional theme hooks. Park continuously changing content geometry and whole-editor deformation for ordinary productivity use. They may suit games, installations, transitions or creative canvases; there is no measured basis for declaring every such use impractical.

</topic>

<topic id="scrub-to-minimap" status="proposed">

## A scrollbar that unfolds into a minimap

Confirmed intended sequence:

1. Idle: show the normal rail and comfortably sized thumb.
2. Grab: expand a minimap beside the document, replacing the rail/thumb presentation.
3. Keep dragging: the held object becomes the viewport rectangle over the miniature document.
4. Move vertically: the viewport rectangle navigates the main document.
5. Release: hide the minimap and restore the normal rail/thumb at the new position.

The important proposed interaction rule is continuity: expansion should not jump to another document position, release the pointer or require a second grab. Preserve the initial grab offset as the visible thumb becomes a larger marker. A stable overview should grow sideways without rewrapping the main document during the drag.

Proposed host settings: trigger mode, expanded width, side, minimum viewport-marker height, preview detail, release delay and always-visible preference. Touch can retain one contact and move the preview away from the finger; keyboard users need an equivalent navigation mode. Cancellation or lost pointer capture must settle the gesture and restore a usable navigation control.

Potential weaknesses: short files do not need an expanded map; enormous files make the proportional viewport marker tiny; unfolding over text can obscure it; dynamically changing documents can invalidate the drag mapping. Candidate mitigations are capability/length gating, minimum marker size with complete range mapping, a bounded side overlay, and a stable provider revision or anchored mapping for the duration of the drag.

For coarse and fine navigation, an optional horizontal drag modifier could adjust sensitivity without changing the selected document anchor. That is an extra proposal, not part of the operator's core gesture. Start evaluation with the original single vertical drag first.

[CodeGlance Pro](https://github.com/Nasller/CodeGlancePro) documents hiding the original scrollbar and a document minimap with highlights. [Kate's scrollbar minimap](https://docs.kde.org/stable_kf6/en/kate/katepart/kate-part-scrollbar-minimap.html) replaces the scrollbar with a miniature document and highlights the visible portion, but its trigger is a setting/menu toggle.

[VS Code minimap options](https://raw.githubusercontent.com/microsoft/vscode/main/src/vs/editor/common/config/editorOptions.ts) support auto-hide during mouseover or scrolling; its [minimap interaction source](https://raw.githubusercontent.com/microsoft/vscode/main/src/vs/editor/browser/viewParts/minimap/minimap.ts) tracks initial pointer/layout state for viewport dragging. [Scrollbar Lens](https://github.com/emvaized/scrollbar_lens_extension) reveals a page map on scrollbar hover. [vv-scrollbar.nvim](https://github.com/beixiyo/vv-scrollbar.nvim) documents classic/map switching and grab-offset preservation. None of the inspected interactions establishes the exact press-to-transform and release-to-revert combination; that remains a Kumihimo proposal.

One alternative for very long files is to keep the viewport marker visually proportional and give it a larger invisible hit area, rather than enlarging the visual marker. Compare this against a minimum visible marker before choosing: accurate scale and easy grabbing are different goals.

Provisional evaluation: retain this as a strong interaction candidate for further prep. It combines a large ordinary thumb with on-demand document context and saves permanent minimap width. No working gesture or usability result is claimed.

Future evaluation scenarios: press at several points within the thumb; drag slowly and rapidly; release outside the window; cancel; use touch/pen; resize during dragging; update the document; navigate a huge file; navigate a short file; use a host without minimap data.

</topic>

<topic id="iteration-boundaries" status="proposed">

## Keep the options open

Suggested next discussion choices: which overlapping-frame use case feels useful; how children should respond when the parent changes size; whether decorative effects signal state or only theme; and whether the minimap replaces a reserved sidebar or temporarily overlays its edge.

The current recommendation is to explore snapping and the transient minimap further, keep border effects optional, and postpone interactive content deformation. This recommendation has not been adopted into the master spec. No builds, installs, GUI implementation or long test runs are part of this prep work.

</topic>
