//! Reaction records (spec STU-PRO-102..107, STU-PRO-110..112, STU-PRO-116..119, STU-PRO-121): a
//! reaction is `{trigger, actions[]}`, an ordered list of them lives on a node (STU-PRO-103), a null
//! trigger is authored but not armed and persists. Folio stores and validates only; playing a
//! prototype, transitions and variable writes belong to the components and motion crates.
//!
//! Declared gaps, not invented here: the `CONDITIONAL` action (it needs the `EXPRESSION` value form)
//! and the `UPDATE_MEDIA_RUNTIME` action with its nine media members (STU-PRO-110/114) are not
//! modelled; the media-time upper bound against the referenced media duration (STU-PRO-108) cannot be
//! checked without the media and is not; warnings of STU-PRO-104/127 (actions after a navigation,
//! `preserve_scroll_position` outside `NAVIGATE`) are not reported because Folio has no warning
//! channel; the overlay position requirement of `NODE` with `OVERLAY` (STU-PRO-112) depends on the
//! destination frame's overlay fields (STU-PRO-122), which are not stored; whether a controller
//! trigger's `device` must equal its own type is not stated by STU-PRO-106 and is not enforced;
//! `duration` of a spring easing is validated like any other (the derived-duration option of
//! STU-PRO-121 is not modelled).
use super::{
    Code, Result, err, id,
    records::Refs,
    values::{Ctx, StudioEasing, StudioValue, label},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// STU-PRO-107 `studio_declared` hard maximum of a timeout or delay, milliseconds.
pub const MAX_TIMEOUT_MS: u32 = 3_600_000;
/// STU-PRO-121 `studio_declared` hard maximum of a transition duration, milliseconds.
pub const MAX_DURATION_MS: u32 = 30_000;
/// Reactions per node and actions per reaction (Folio hard ceilings; the spec declares none).
pub const MAX_REACTIONS_PER_NODE: usize = 256;
pub const MAX_ACTIONS_PER_REACTION: usize = 64;
/// Key codes per trigger (a Folio hard ceiling).
pub const MAX_KEY_CODES: usize = 64;
/// Longest `URL` action target.
const MAX_URL_BYTES: usize = 2048;

/// STU-PRO-106 `device`: the controller members plus `KEYBOARD`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum InputDevice {
    Keyboard,
    XboxOne,
    Ps4,
    SwitchPro,
    UnknownController,
}

/// STU-PRO-106 drag-axis constraint; absent means `BOTH`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DragAxis {
    Horizontal,
    Vertical,
    Both,
}

/// STU-PRO-105 trigger types (seventeen) with the STU-PRO-106 type-specific members. Times are
/// milliseconds (STU-PRO-107, Studio-declared unit).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE", deny_unknown_fields)]
pub enum StudioTrigger {
    OnClick {},
    OnDrag {
        delay: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        drag_axis: Option<DragAxis>,
    },
    OnHover {},
    OnPress {},
    MouseEnter {},
    MouseLeave {},
    MouseDown {},
    MouseUp {},
    AfterTimeout {
        timeout: u32,
    },
    Keyboard {
        device: InputDevice,
        key_codes: Vec<u32>,
    },
    OnKeyDown {
        device: InputDevice,
        key_codes: Vec<u32>,
    },
    OnMediaHit {
        media_hit_time: u64,
    },
    OnMediaEnd {},
    XboxOne {
        device: InputDevice,
        key_codes: Vec<u32>,
    },
    Ps4 {
        device: InputDevice,
        key_codes: Vec<u32>,
    },
    SwitchPro {
        device: InputDevice,
        key_codes: Vec<u32>,
    },
    UnknownController {
        device: InputDevice,
        key_codes: Vec<u32>,
    },
}

/// STU-PRO-111 navigation sub-kind of a `NODE` action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Navigation {
    Navigate,
    ChangeTo,
    Overlay,
    Swap,
    ScrollTo,
}

/// STU-PRO-117/118 transition types: the first three are simple, the other five directional.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TransitionType {
    Dissolve,
    SmartAnimate,
    ScrollAnimate,
    MoveIn,
    MoveOut,
    Push,
    SlideIn,
    SlideOut,
}
impl TransitionType {
    pub fn is_directional(self) -> bool {
        matches!(
            self,
            Self::MoveIn | Self::MoveOut | Self::Push | Self::SlideIn | Self::SlideOut
        )
    }
}

/// STU-PRO-118 direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TransitionDirection {
    Left,
    Right,
    Top,
    Bottom,
}

/// STU-PRO-116 transition: a simple record (`{type, easing, duration}`) or a directional one
/// (`{type, direction, match_layers, easing, duration}`); instant is a null transition on the action.
/// `direction` and `match_layers` are present exactly for the directional types.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioTransition {
    #[serde(rename = "type")]
    pub transition_type: TransitionType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<TransitionDirection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_layers: Option<bool>,
    pub easing: StudioEasing,
    /// Milliseconds, `0..=30000`.
    pub duration: u32,
}

/// `NODE` overlay position relative to the origin frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OverlayPosition {
    pub x: f64,
    pub y: f64,
}

/// STU-PRO-112 action records (the modelled subset of the seventeen STU-PRO-110 types).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE", deny_unknown_fields)]
pub enum StudioAction {
    Back {},
    Close {},
    Url {
        url: String,
        open_in_new_tab: bool,
    },
    Node {
        /// A node of the document, or explicit null.
        #[serde(deserialize_with = "super::required")]
        #[schemars(required)]
        destination_id: Option<String>,
        navigation: Navigation,
        /// Explicit null is an instant transition.
        #[serde(deserialize_with = "super::required")]
        #[schemars(required)]
        transition: Option<StudioTransition>,
        preserve_scroll_position: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        overlay_relative_position: Option<OverlayPosition>,
        reset_video_position: bool,
        reset_scroll_position: bool,
        reset_interactive_components: bool,
    },
    /// Writes the running prototype's variable state, never the document (STU-PRO-113).
    SetVariable {
        variable_id: String,
        variable_value: StudioValue,
    },
    SetVariableMode {
        variable_collection_id: String,
        variable_mode_id: String,
    },
}

/// STU-PRO-102 reaction: an ordered action list behind a trigger; a null trigger is not armed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioReaction {
    #[serde(deserialize_with = "super::required")]
    #[schemars(required)]
    pub trigger: Option<StudioTrigger>,
    pub actions: Vec<StudioAction>,
}

fn trigger(t: &StudioTrigger, target: &str) -> Result<()> {
    let bad = || err(Code::InvalidField, format!("{target}/trigger"));
    let keys = |codes: &[u32]| (1..=MAX_KEY_CODES).contains(&codes.len());
    let ok = match t {
        StudioTrigger::OnDrag { delay, .. } => *delay <= MAX_TIMEOUT_MS,
        StudioTrigger::AfterTimeout { timeout } => *timeout <= MAX_TIMEOUT_MS,
        StudioTrigger::Keyboard { key_codes, .. }
        | StudioTrigger::OnKeyDown { key_codes, .. }
        | StudioTrigger::XboxOne { key_codes, .. }
        | StudioTrigger::Ps4 { key_codes, .. }
        | StudioTrigger::SwitchPro { key_codes, .. }
        | StudioTrigger::UnknownController { key_codes, .. } => keys(key_codes),
        _ => true,
    };
    if ok { Ok(()) } else { Err(bad()) }
}

fn transition(t: &StudioTransition, target: &str) -> Result<()> {
    let target = format!("{target}/transition");
    let directional = t.transition_type.is_directional();
    if directional != (t.direction.is_some() && t.match_layers.is_some())
        || (!directional && (t.direction.is_some() || t.match_layers.is_some()))
        || t.duration > MAX_DURATION_MS
    {
        return Err(err(Code::InvalidField, target));
    }
    t.easing.validate(&target)
}

fn action(a: &StudioAction, target: &str, refs: &Refs, c: &mut Ctx) -> Result<()> {
    match a {
        StudioAction::Back {} | StudioAction::Close {} => Ok(()),
        StudioAction::Url { url, .. } => {
            if url.is_empty() || url.len() > MAX_URL_BYTES || url.chars().any(char::is_control) {
                return Err(err(Code::InvalidField, format!("{target}/url")));
            }
            c.text(url, target)
        }
        StudioAction::Node {
            destination_id,
            transition: tr,
            overlay_relative_position,
            ..
        } => {
            if let Some(d) = destination_id
                && !refs.nodes.contains(d.as_str())
            {
                return Err(err(
                    Code::DanglingReference,
                    format!("{target}/destination_id"),
                ));
            }
            if overlay_relative_position
                .as_ref()
                .is_some_and(|p| !p.x.is_finite() || !p.y.is_finite())
            {
                return Err(err(
                    Code::InvalidField,
                    format!("{target}/overlay_relative_position"),
                ));
            }
            tr.as_ref().map_or(Ok(()), |t| transition(t, target))
        }
        StudioAction::SetVariable {
            variable_id,
            variable_value,
        } => {
            let vtarget = format!("{target}/variable_id");
            id(variable_id, "SVAR", &vtarget)?;
            let variable = refs
                .variables
                .get(variable_id.as_str())
                .ok_or_else(|| err(Code::DanglingReference, &vtarget))?;
            variable_value.validate(&format!("{target}/variable_value"), c)?;
            if variable_value.resolved_type() != variable.resolved_type {
                return Err(err(Code::InvalidField, format!("{target}/variable_value")));
            }
            Ok(())
        }
        StudioAction::SetVariableMode {
            variable_collection_id,
            variable_mode_id,
        } => {
            let ctarget = format!("{target}/variable_collection_id");
            id(variable_collection_id, "SVCL", &ctarget)?;
            let collection = refs
                .collections
                .get(variable_collection_id.as_str())
                .ok_or_else(|| err(Code::DanglingReference, &ctarget))?;
            if !label(variable_mode_id)
                || !collection
                    .modes
                    .iter()
                    .any(|m| &m.mode_id == variable_mode_id)
            {
                return Err(err(
                    Code::DanglingReference,
                    format!("{target}/variable_mode_id"),
                ));
            }
            Ok(())
        }
    }
}

/// Validates the ordered reaction list of one node. Destinations must be nodes of the document,
/// variable and collection references must be records of the same design records.
pub(crate) fn validate_reactions(
    reactions: &[StudioReaction],
    owner: &str,
    refs: &Refs,
    c: &mut Ctx,
) -> Result<()> {
    if reactions.len() > MAX_REACTIONS_PER_NODE {
        return Err(err(Code::BudgetExceeded, format!("{owner}/reactions")));
    }
    for (n, r) in reactions.iter().enumerate() {
        c.tick()?;
        let rtarget = format!("{owner}/reactions/{n}");
        if r.actions.len() > MAX_ACTIONS_PER_REACTION {
            return Err(err(Code::BudgetExceeded, format!("{rtarget}/actions")));
        }
        c.nodes(1 + r.actions.len(), &rtarget)?;
        if let Some(t) = &r.trigger {
            trigger(t, &rtarget)?;
        }
        for (m, a) in r.actions.iter().enumerate() {
            action(a, &format!("{rtarget}/actions/{m}"), refs, c)?;
        }
    }
    Ok(())
}
