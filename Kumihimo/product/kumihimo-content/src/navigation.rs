//! Host-fed navigation policy. Rendering, text layout and native gesture recognition stay in adapters.
use crate::routing::State;
use crate::*;
use kumihimo_core::{Core, Rect, Scalar};

pub(crate) fn number(value: f64) -> Result<Scalar> {
    if !value.is_finite() {
        return Err(Error::Invalid("navigation arithmetic"));
    }
    Ok(Scalar::new(value)?)
}
pub(crate) fn nonnegative(value: Scalar) -> Result<()> {
    if value.get() < 0.0 {
        Err(Error::Invalid("negative navigation metric"))
    } else {
        Ok(())
    }
}
fn positive(value: Scalar) -> Result<()> {
    if value.get() <= 0.0 {
        Err(Error::Invalid("nonpositive navigation dimension"))
    } else {
        Ok(())
    }
}
pub(crate) fn token(value: &str, limit: Option<usize>) -> Result<()> {
    if let Some(limit) = limit {
        if value.len() > limit {
            return Err(Error::Admission {
                dimension: "navigation token bytes",
                limit,
                attempted: value.len(),
            });
        }
    }
    Ok(())
}
pub(crate) fn increasing(revision: u64, previous: u64, identical: bool) -> Result<bool> {
    if revision == 0 || revision == u64::MAX {
        return Err(Error::Invalid("navigation revision"));
    }
    if revision < previous {
        return Err(Error::Stale("navigation revision"));
    }
    if revision == previous {
        return if identical {
            Ok(false)
        } else {
            Err(Error::Invalid("navigation revision conflict"))
        };
    }
    Ok(true)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum NavigationMode {
    ScrollbarOnly,
    MinimapAlways,
    ScrollbarAndMinimap,
    ScrubToMinimap,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigationFallback {
    ScrollbarOnly,
    Unavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigationDisabled {
    PolicyDenied,
    NavigationUnsupported,
    InputUnsupported,
    OverviewUnsupported,
    OverviewMissing,
    ModelMissing,
    ActualUnknown,
    RailMissing,
    NoRange,
    NoPointerTravel,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlProfile {
    Standard,
    Comfortable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RailVisibility {
    AlwaysVisible,
    AutoHide,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerKind {
    Mouse,
    Touch,
    Pen,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NavigationCapabilities {
    pub scroll: bool,
    pub overview: bool,
}
impl Default for NavigationCapabilities {
    fn default() -> Self {
        Self {
            scroll: true,
            overview: true,
        }
    }
}
impl NavigationCapabilities {
    pub fn intersect(&self, other: &Self) -> Self {
        Self {
            scroll: self.scroll && other.scroll,
            overview: self.overview && other.overview,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NavigationSettingsPatch {
    pub mode: Option<NavigationMode>,
    pub profile: Option<ControlProfile>,
    pub rail_width: Option<Scalar>,
    pub thumb_width: Option<Scalar>,
    pub minimum_thumb: Option<Scalar>,
    pub hit_width: Option<Scalar>,
    pub contrast: Option<Scalar>,
    pub visibility: Option<RailVisibility>,
    pub line_step: Option<Scalar>,
    pub accessible_name: Option<String>,
    pub mouse_threshold: Option<Scalar>,
    pub touch_threshold: Option<Scalar>,
    pub pen_threshold: Option<Scalar>,
    pub transition: Option<Transition>,
}
impl NavigationSettingsPatch {
    fn validate(&self) -> Result<()> {
        for value in [
            self.rail_width,
            self.thumb_width,
            self.minimum_thumb,
            self.hit_width,
            self.line_step,
        ]
        .into_iter()
        .flatten()
        {
            positive(value)?;
        }
        for value in [
            self.mouse_threshold,
            self.touch_threshold,
            self.pen_threshold,
        ]
        .into_iter()
        .flatten()
        {
            nonnegative(value)?;
        }
        if self
            .contrast
            .is_some_and(|v| !(0.0..=1.0).contains(&v.get()))
        {
            return Err(Error::Invalid("navigation contrast"));
        }
        if let Some(name) = &self.accessible_name {
            if name.is_empty() {
                return Err(Error::Invalid("accessible name"));
            }
            token(name, Some(256))?;
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavigationSettings {
    pub version: u16,
    pub mode: Resolved<NavigationMode>,
    pub profile: Resolved<ControlProfile>,
    pub rail_width: Resolved<Scalar>,
    pub thumb_width: Resolved<Scalar>,
    pub minimum_thumb: Resolved<Scalar>,
    pub hit_width: Resolved<Scalar>,
    pub contrast: Resolved<Scalar>,
    pub visibility: Resolved<RailVisibility>,
    pub line_step: Resolved<Scalar>,
    pub accessible_name: Resolved<String>,
    pub mouse_threshold: Resolved<Scalar>,
    pub touch_threshold: Resolved<Scalar>,
    pub pen_threshold: Resolved<Scalar>,
    pub transition: Resolved<Transition>,
}
fn default_value<T>(value: T) -> Resolved<T> {
    Resolved {
        value,
        source: SettingSource::Default,
    }
}
impl Default for NavigationSettings {
    fn default() -> Self {
        Self {
            version: 2,
            mode: default_value(NavigationMode::ScrollbarOnly),
            profile: default_value(ControlProfile::Standard),
            rail_width: default_value(number(12.0).unwrap()),
            thumb_width: default_value(number(8.0).unwrap()),
            minimum_thumb: default_value(number(24.0).unwrap()),
            hit_width: default_value(number(24.0).unwrap()),
            contrast: default_value(number(1.0).unwrap()),
            visibility: default_value(RailVisibility::AlwaysVisible),
            line_step: default_value(number(40.0).unwrap()),
            accessible_name: default_value("Document position".into()),
            mouse_threshold: default_value(number(0.0).unwrap()),
            touch_threshold: default_value(number(0.0).unwrap()),
            pen_threshold: default_value(number(0.0).unwrap()),
            transition: default_value(Transition::Live),
        }
    }
}
pub fn resolve_navigation_settings(
    layers: &SettingsLayers<NavigationSettingsPatch>,
) -> Result<NavigationSettings> {
    layers.validate_version()?;
    let mut settings = NavigationSettings::default();
    settings.transition.value = layers.transition;
    for (patch, source) in [
        (&layers.host_profile, SettingSource::HostProfile),
        (&layers.workspace, SettingSource::Workspace),
        (&layers.surface, SettingSource::Surface),
    ] {
        patch.validate()?;
        if let Some(profile) = patch.profile {
            settings.profile = Resolved {
                value: profile,
                source,
            };
            let values = match profile {
                ControlProfile::Standard => [12.0, 8.0, 24.0, 24.0],
                ControlProfile::Comfortable => [24.0, 16.0, 48.0, 40.0],
            };
            settings.rail_width = Resolved {
                value: number(values[0])?,
                source,
            };
            settings.thumb_width = Resolved {
                value: number(values[1])?,
                source,
            };
            settings.minimum_thumb = Resolved {
                value: number(values[2])?,
                source,
            };
            settings.hit_width = Resolved {
                value: number(values[3])?,
                source,
            };
        }
        macro_rules! apply { ($($field:ident),*) => {$ (if let Some(value)=&patch.$field { settings.$field=Resolved{value:value.clone(),source}; })*}; }
        apply!(
            mode,
            rail_width,
            thumb_width,
            minimum_thumb,
            hit_width,
            contrast,
            visibility,
            line_step,
            accessible_name,
            mouse_threshold,
            touch_threshold,
            pen_threshold,
            transition
        );
    }
    Ok(settings)
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavigationPolicy {
    pub allowed: bool,
    pub overview_allowed: bool,
    pub fallback: NavigationFallback,
}
impl Default for NavigationPolicy {
    fn default() -> Self {
        Self {
            allowed: true,
            overview_allowed: true,
            fallback: NavigationFallback::ScrollbarOnly,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentModel {
    pub revision: u64,
    pub semantic: String,
    pub unit: String,
    pub extent: Scalar,
    pub viewport: Scalar,
}
impl DocumentModel {
    pub fn validate(&self, field_limit: Option<usize>) -> Result<()> {
        increasing(self.revision, 0, false)?;
        nonnegative(self.extent)?;
        nonnegative(self.viewport)?;
        if self.semantic.is_empty() || self.unit.is_empty() {
            return Err(Error::Invalid("document model identity"));
        }
        for value in [&self.semantic, &self.unit] {
            token(value, Some(256))?;
            token(value, field_limit)?;
        }
        self.range()?;
        Ok(())
    }
    pub fn range(&self) -> Result<Scalar> {
        number((self.extent.get() - self.viewport.get()).max(0.0))
    }
    fn validate_offset(&self, offset: Scalar) -> Result<()> {
        if offset.get() < 0.0 || offset.get() > self.range()?.get() {
            Err(Error::Invalid("document offset"))
        } else {
            Ok(())
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavigationObservation {
    pub model_revision: u64,
    pub revision: u64,
    pub offset: Scalar,
}
impl NavigationObservation {
    pub fn validate(&self) -> Result<()> {
        increasing(self.model_revision, 0, false)?;
        increasing(self.revision, 0, false)?;
        nonnegative(self.offset)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RailGeometry {
    pub origin: Scalar,
    pub length: Scalar,
    pub map_origin: Scalar,
    pub map_length: Scalar,
    pub document_rect: Rect,
}
impl RailGeometry {
    pub fn validate(&self) -> Result<()> {
        for v in [self.origin, self.length, self.map_origin, self.map_length] {
            nonnegative(v)?;
        }
        number(self.origin.get() + self.length.get())?;
        number(self.map_origin.get() + self.map_length.get())?;
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThumbGeometry {
    pub start: Scalar,
    pub length: Scalar,
    pub travel: Scalar,
    pub progress: Scalar,
    pub pointer_available: bool,
}
pub fn navigation_thumb(
    model: &DocumentModel,
    offset: Scalar,
    origin: Scalar,
    track: Scalar,
    minimum: Scalar,
) -> Result<ThumbGeometry> {
    model.validate(None)?;
    model.validate_offset(offset)?;
    nonnegative(origin)?;
    nonnegative(track)?;
    positive(minimum)?;
    number(origin.get() + track.get())?;
    let range = model.range()?.get();
    let ratio = if model.extent.get() == 0.0 || model.viewport.get() >= model.extent.get() {
        1.0
    } else {
        model.viewport.get() / model.extent.get()
    };
    let length = track.get().min(minimum.get().max(track.get() * ratio));
    let travel = track.get() - length;
    let progress = if range == 0.0 {
        0.0
    } else {
        offset.get() / range
    };
    Ok(ThumbGeometry {
        start: number(origin.get() + travel * progress)?,
        length: number(length)?,
        travel: number(travel)?,
        progress: number(progress)?,
        pointer_available: range > 0.0 && travel > 0.0,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NavigationAction {
    AbsoluteScroll(Scalar),
    SemanticJump {
        overview_revision: u64,
        target: String,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavigationRequest {
    pub model_revision: u64,
    pub settings_revision: u64,
    pub rail_revision: u64,
    pub gesture: Option<u64>,
    pub action: NavigationAction,
}
impl NavigationRequest {
    pub fn validate(&self, limit: Option<usize>) -> Result<()> {
        for revision in [
            self.model_revision,
            self.settings_revision,
            self.rail_revision,
        ] {
            increasing(revision, 0, false)?;
        }
        if let Some(g) = self.gesture {
            increasing(g, 0, false)?;
        }
        match &self.action {
            NavigationAction::AbsoluteScroll(v) => nonnegative(*v)?,
            NavigationAction::SemanticJump {
                overview_revision,
                target,
            } => {
                increasing(*overview_revision, 0, false)?;
                if target.is_empty() {
                    return Err(Error::Invalid("jump target"));
                }
                token(target, limit)?;
            }
        }
        Ok(())
    }
    pub fn opaque_bytes(&self) -> Result<usize> {
        Ok(match &self.action {
            NavigationAction::SemanticJump { target, .. } => target.len(),
            _ => 0,
        })
    }
    pub fn barrier(&self) -> bool {
        self.gesture.is_none() || !matches!(self.action, NavigationAction::AbsoluteScroll(_))
    }
    pub fn coalesces_with(&self, other: &Self) -> bool {
        !self.barrier()
            && !other.barrier()
            && self.model_revision == other.model_revision
            && self.settings_revision == other.settings_revision
            && self.rail_revision == other.rail_revision
            && self.gesture == other.gesture
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GestureSettlement {
    Release,
    Cancel,
    Escape,
    CaptureLost,
    FocusLost,
    Invalidated,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettledGesture {
    pub gesture: u64,
    pub pointer: u64,
    pub reason: GestureSettlement,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavigationGesture {
    pub id: u64,
    pub pointer: u64,
    pub kind: PointerKind,
    pub scope: Scope,
    pub model_revision: u64,
    pub settings_revision: u64,
    pub rail_revision: u64,
    pub geometry_revision: u64,
    pub focus_revision: u64,
    pub press: Scalar,
    pub last_pointer: Scalar,
    pub initial_offset: Scalar,
    pub intended_offset: Scalar,
    pub grab_offset: Scalar,
    pub presentation_anchor: Scalar,
    pub travel: Scalar,
    pub document_rect: Rect,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavigationState {
    pub scope: Scope,
    pub settings_revision: u64,
    pub settings: NavigationSettings,
    pub policy: NavigationPolicy,
    pub model: Option<DocumentModel>,
    pub observation: Observation<NavigationObservation>,
    /// Retained independently of Unknown so stale/duplicate host observations stay detectable.
    pub last_observation: Option<NavigationObservation>,
    pub rail_revision: u64,
    pub rail: Option<RailGeometry>,
    pub overview: Option<Overview>,
    pub last_overview_revision: u64,
    pub gesture: Option<NavigationGesture>,
    pub last_gesture: u64,
    pub settled: Option<SettledGesture>,
    pub hover: bool,
    pub focused: bool,
    pub revealed: bool,
    pub last_request: Option<RequestId>,
    pub last_intent: Option<Scalar>,
}
impl NavigationState {
    fn new(scope: Scope) -> Self {
        Self {
            scope,
            settings_revision: 1,
            settings: NavigationSettings::default(),
            policy: NavigationPolicy::default(),
            model: None,
            observation: Observation::Unknown,
            last_observation: None,
            rail_revision: 0,
            rail: None,
            overview: None,
            last_overview_revision: 0,
            gesture: None,
            last_gesture: 0,
            settled: None,
            hover: false,
            focused: false,
            revealed: false,
            last_request: None,
            last_intent: None,
        }
    }
    pub fn opaque_bytes(&self) -> Result<usize> {
        // Scope tokens are existing bounded protocol metadata; this counter charges opaque payloads.
        let mut n = self.settings.accessible_name.value.len();
        if let Some(model) = &self.model {
            n = n
                .checked_add(model.semantic.len())
                .and_then(|n| n.checked_add(model.unit.len()))
                .ok_or(Error::Exhausted("navigation bytes"))?;
        }
        if let Some(overview) = &self.overview {
            n = n
                .checked_add(overview.opaque_bytes()?)
                .ok_or(Error::Exhausted("navigation bytes"))?;
        }
        Ok(n)
    }
    pub fn overview_marker_count(&self) -> usize {
        self.overview.as_ref().map_or(0, |o| o.markers.len())
    }
    pub(crate) fn effective(
        &self,
        caps: &Capabilities,
    ) -> (Option<NavigationMode>, Option<NavigationDisabled>) {
        let reason = if !self.policy.allowed {
            Some(NavigationDisabled::PolicyDenied)
        } else if !caps.navigation.scroll {
            Some(NavigationDisabled::NavigationUnsupported)
        } else if !caps.input {
            Some(NavigationDisabled::InputUnsupported)
        } else if self.settings.mode.value != NavigationMode::ScrollbarOnly {
            if !self.policy.overview_allowed || !caps.navigation.overview {
                Some(NavigationDisabled::OverviewUnsupported)
            } else if self.overview.is_none() {
                Some(NavigationDisabled::OverviewMissing)
            } else {
                None
            }
        } else {
            None
        };
        if let Some(reason) = reason {
            let fallback = if self.policy.allowed
                && caps.navigation.scroll
                && caps.input
                && self.policy.fallback == NavigationFallback::ScrollbarOnly
            {
                Some(NavigationMode::ScrollbarOnly)
            } else {
                None
            };
            (fallback, Some(reason))
        } else {
            (Some(self.settings.mode.value), None)
        }
    }
    fn current_observation(&self) -> Result<&NavigationObservation> {
        match &self.observation {
            Observation::Known(o) => Ok(o),
            _ => Err(Error::Stale("actual offset unknown")),
        }
    }
    fn validate_observation(&self, o: &NavigationObservation) -> Result<bool> {
        o.validate()?;
        let model = self
            .model
            .as_ref()
            .ok_or(Error::Stale("document model missing"))?;
        if model.revision != o.model_revision {
            return Err(Error::Stale("offset model revision"));
        }
        model.validate_offset(o.offset)?;
        let prior = self.last_observation.as_ref();
        increasing(
            o.revision,
            prior.map_or(0, |p| p.revision),
            prior == Some(o),
        )
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavigationProjection {
    pub settings_version: u16,
    pub selected: NavigationMode,
    pub effective: Option<NavigationMode>,
    pub fallback_reason: Option<NavigationDisabled>,
    pub rail_visible: bool,
    pub minimap_visible: bool,
    pub held_viewport: bool,
    pub gesture: Option<u64>,
    pub model_revision: Option<u64>,
    pub observation: Observation<NavigationObservation>,
    pub requested_offset: Option<Scalar>,
    pub thumb: Option<ThumbGeometry>,
    pub viewport_rectangle: Option<ThumbGeometry>,
    pub document_rect: Option<Rect>,
    pub presentation_anchor: Option<Scalar>,
    pub rail_width: Scalar,
    pub thumb_width: Scalar,
    pub hit_width: Scalar,
    pub contrast: Scalar,
    pub orientation: &'static str,
    pub role: &'static str,
    pub accessible_name: String,
    pub disabled_reason: Option<NavigationDisabled>,
    pub minimum: Scalar,
    pub maximum: Option<Scalar>,
    pub value: Option<Scalar>,
    pub transition: Transition,
    pub last_request: Option<RequestId>,
    pub request_status: Option<RequestStatus>,
}

impl State {
    pub(crate) fn navigation_scope(&self, core: &Core, scope: &Scope) -> Result<()> {
        scope.validate()?;
        let m = self
            .mounts
            .get(&scope.mount)
            .ok_or(Error::Missing("mount", scope.mount.0))?;
        if m.scope != *scope
            || !m.active
            || !m.reserved
            || !self.binding_valid(core.snapshot(), scope)
        {
            return Err(Error::Stale("navigation binding"));
        }
        if !matches!(scope.target, MountTarget::View(_)) {
            return Err(Error::Unsupported("navigation target"));
        }
        Ok(())
    }
    fn ensure_navigation(&mut self, scope: &Scope) -> &mut NavigationState {
        self.navigation
            .entry(scope.mount)
            .or_insert_with(|| NavigationState::new(scope.clone()))
    }
    pub(crate) fn navigation_eligible(&self, mount: MountId, r: &NavigationRequest) -> Result<()> {
        // Enqueue validates current caps; old admitted obligations survive later cap lowering.
        r.validate(None)?;
        let n = self
            .navigation
            .get(&mount)
            .ok_or(Error::Stale("navigation not configured"))?;
        let caps = &self.providers[&n.scope.provider].capabilities;
        if n.effective(caps).0.is_none() {
            return Err(Error::Unsupported("navigation disabled"));
        }
        if n.model
            .as_ref()
            .is_none_or(|m| m.revision != r.model_revision)
            || n.settings_revision != r.settings_revision
            || n.rail_revision != r.rail_revision
        {
            return Err(Error::Stale("navigation prerequisites"));
        }
        if let Some(gesture) = r.gesture {
            let active = n.gesture.as_ref().is_some_and(|g| g.id == gesture);
            let released = n
                .settled
                .as_ref()
                .is_some_and(|g| g.gesture == gesture && g.reason == GestureSettlement::Release);
            let admitted = self.pending.values().any(|p| {
                p.request.ticket.scope.mount == mount
                    && !p.stale
                    && matches!(p.status, RequestStatus::Queued | RequestStatus::Dispatched)
                    && matches!(&p.request.kind,RequestKind::Navigation(request) if request==r)
            });
            if !active && !released && !admitted {
                return Err(Error::Stale("navigation request gesture"));
            }
        }
        match &r.action {
            NavigationAction::AbsoluteScroll(offset) => {
                n.model.as_ref().unwrap().validate_offset(*offset)?
            }
            NavigationAction::SemanticJump {
                overview_revision,
                target,
            } => {
                let overview = n
                    .overview
                    .as_ref()
                    .ok_or(Error::Stale("overview missing"))?;
                if overview.revision != *overview_revision
                    || overview.model_revision != r.model_revision
                    || !overview
                        .markers
                        .iter()
                        .any(|m| m.jump_target.as_ref() == Some(target))
                {
                    return Err(Error::Stale("semantic jump target"));
                }
            }
        }
        Ok(())
    }
    pub(crate) fn navigation_validate_confirmation(
        &self,
        mount: MountId,
        r: &NavigationRequest,
        o: &NavigationObservation,
    ) -> Result<()> {
        self.navigation_eligible(mount, r)?;
        if r.model_revision != o.model_revision {
            return Err(Error::Stale("navigation acknowledgment model"));
        }
        self.navigation[&mount].validate_observation(o)?;
        Ok(())
    }
    pub(crate) fn navigation_apply(
        &mut self,
        mount: MountId,
        o: &NavigationObservation,
    ) -> Result<()> {
        let n = self
            .navigation
            .get_mut(&mount)
            .ok_or(Error::Stale("navigation state"))?;
        n.validate_observation(o)?;
        n.last_observation = Some(o.clone());
        n.observation = Observation::Known(o.clone());
        Ok(())
    }
    pub(crate) fn navigation_unknown(&mut self, mount: MountId) {
        if let Some(n) = self.navigation.get_mut(&mount) {
            n.observation = Observation::Unknown;
        }
    }
    fn settle_navigation(&mut self, mount: MountId, settled: SettledGesture) -> Result<()> {
        let n = self
            .navigation
            .get_mut(&mount)
            .ok_or(Error::Stale("navigation state"))?;
        if n.gesture.is_none() {
            return if n.settled.as_ref() == Some(&settled) {
                Ok(())
            } else {
                Err(Error::Stale("settled navigation gesture"))
            };
        }
        let g = n.gesture.as_ref().unwrap();
        if g.id != settled.gesture || g.pointer != settled.pointer {
            return Err(Error::Stale("navigation gesture identity"));
        }
        n.gesture = None;
        n.settled = Some(settled.clone());
        if settled.reason != GestureSettlement::Release {
            let ids:Vec<_>=self.pending.iter().filter(|(_,p)|p.request.ticket.scope.mount==mount && matches!(&p.request.kind,RequestKind::Navigation(r) if r.gesture==Some(settled.gesture))).map(|(id,_)|*id).collect();
            for id in ids {
                let p = self.pending[&id].clone();
                if p.status == RequestStatus::Queued {
                    self.remove_request(id);
                    self.receipt(
                        p,
                        RequestStatus::Cancelled,
                        None,
                        Some("navigation gesture settled"),
                    );
                } else {
                    self.unknown(id);
                    self.navigation_unknown(mount);
                }
            }
        }
        Ok(())
    }
    pub(crate) fn navigation_invalidate(&mut self, mount: MountId) {
        if let Some(g) = self.navigation.get(&mount).and_then(|n| n.gesture.clone()) {
            let _ = self.settle_navigation(
                mount,
                SettledGesture {
                    gesture: g.id,
                    pointer: g.pointer,
                    reason: GestureSettlement::Invalidated,
                },
            );
        }
        let dispatched = self.pending.values().any(|p| {
            p.request.ticket.scope.mount == mount
                && matches!(p.request.kind, RequestKind::Navigation(_))
                && p.status != RequestStatus::Queued
        });
        self.stale_where(|p| {
            p.request.ticket.scope.mount == mount
                && matches!(p.request.kind, RequestKind::Navigation(_))
        });
        if dispatched {
            self.navigation_unknown(mount);
        }
    }
}

impl Controller {
    pub fn configure_navigation(
        &mut self,
        scope: &Scope,
        revision: u64,
        layers: SettingsLayers<NavigationSettingsPatch>,
        policy: NavigationPolicy,
    ) -> Result<()> {
        let settings = resolve_navigation_settings(&layers)?;
        self.transaction(|core, s| {
            s.navigation_scope(core, scope)?;
            token(&settings.accessible_name.value, s.limits.payload_bytes)?;
            let n = s.ensure_navigation(scope);
            if !increasing(
                revision,
                n.settings_revision,
                n.settings == settings && n.policy == policy,
            )? {
                return Ok(());
            }
            s.navigation_invalidate(scope.mount);
            let n = s.navigation.get_mut(&scope.mount).unwrap();
            n.settings_revision = revision;
            n.settings = settings;
            n.policy = policy;
            Ok(())
        })
    }
    pub fn update_navigation(
        &mut self,
        scope: &Scope,
        model: DocumentModel,
        observation: NavigationObservation,
    ) -> Result<()> {
        self.transaction(|core, s| {
            s.navigation_scope(core, scope)?;
            model.validate(s.limits.payload_bytes)?;
            let n = s.ensure_navigation(scope);
            let changed = increasing(
                model.revision,
                n.model.as_ref().map_or(0, |m| m.revision),
                n.model.as_ref() == Some(&model),
            )?;
            if changed {
                s.navigation_invalidate(scope.mount);
                let n = s.navigation.get_mut(&scope.mount).unwrap();
                n.model = Some(model);
                n.overview = None;
                n.observation = Observation::Unknown;
                n.last_intent = None;
            }
            s.navigation_apply(scope.mount, &observation)
        })
    }
    pub fn observe_navigation(
        &mut self,
        scope: &Scope,
        observation: NavigationObservation,
    ) -> Result<()> {
        self.transaction(|core, s| {
            s.navigation_scope(core, scope)?;
            s.navigation_apply(scope.mount, &observation)
        })
    }
    pub fn update_navigation_rail(
        &mut self,
        scope: &Scope,
        revision: u64,
        rail: RailGeometry,
    ) -> Result<()> {
        rail.validate()?;
        self.transaction(|core, s| {
            s.navigation_scope(core, scope)?;
            let n = s.ensure_navigation(scope);
            if !increasing(revision, n.rail_revision, n.rail.as_ref() == Some(&rail))? {
                return Ok(());
            }
            s.navigation_invalidate(scope.mount);
            let n = s.navigation.get_mut(&scope.mount).unwrap();
            n.rail_revision = revision;
            n.rail = Some(rail);
            Ok(())
        })
    }
    pub fn navigation_reveal(
        &mut self,
        scope: &Scope,
        hover: bool,
        focused: bool,
        revealed: bool,
    ) -> Result<()> {
        self.transaction(|core, s| {
            s.navigation_scope(core, scope)?;
            let n = s.ensure_navigation(scope);
            n.hover = hover;
            n.focused = focused;
            n.revealed = revealed;
            Ok(())
        })
    }
    pub fn navigation_press(
        &mut self,
        scope: &Scope,
        gesture: u64,
        pointer: u64,
        kind: PointerKind,
        position: Scalar,
    ) -> Result<()> {
        self.transaction(|core, s| {
            s.navigation_scope(core, scope)?;
            if pointer == 0 || pointer == u64::MAX {
                return Err(Error::Invalid("pointer identity"));
            }
            let n = s
                .navigation
                .get(&scope.mount)
                .ok_or(Error::Stale("navigation state"))?;
            if n.gesture.is_some() {
                return Err(Error::Invalid("navigation gesture active"));
            }
            increasing(gesture, n.last_gesture, false)?;
            let mode = n
                .effective(&s.providers[&scope.provider].capabilities)
                .0
                .ok_or(Error::Unsupported("navigation disabled"))?;
            let model = n
                .model
                .as_ref()
                .ok_or(Error::Stale("document model missing"))?;
            let observed = n.current_observation()?;
            let rail = n
                .rail
                .as_ref()
                .ok_or(Error::Stale("navigation rail missing"))?;
            let ordinary = navigation_thumb(
                model,
                observed.offset,
                rail.origin,
                rail.length,
                n.settings.minimum_thumb.value,
            )?;
            let map = navigation_thumb(
                model,
                observed.offset,
                rail.map_origin,
                rail.map_length,
                number(f64::MIN_POSITIVE)?,
            )?;
            let thumb = if mode == NavigationMode::MinimapAlways {
                map.clone()
            } else {
                ordinary
            };
            if !thumb.pointer_available {
                return Err(Error::Unsupported("pointer mapping unavailable"));
            }
            if position.get() < thumb.start.get()
                || position.get() > thumb.start.get() + thumb.length.get()
            {
                return Err(Error::Invalid("press outside thumb"));
            }
            let held = if matches!(
                mode,
                NavigationMode::ScrubToMinimap | NavigationMode::MinimapAlways
            ) {
                &map
            } else {
                &thumb
            };
            if !held.pointer_available {
                return Err(Error::Unsupported("viewport pointer mapping unavailable"));
            }
            let grab = position.get() - thumb.start.get();
            let fraction = if thumb.length.get() == 0.0 {
                0.0
            } else {
                grab / thumb.length.get()
            };
            let m = &s.mounts[&scope.mount];
            let g = NavigationGesture {
                id: gesture,
                pointer,
                kind,
                scope: scope.clone(),
                model_revision: model.revision,
                settings_revision: n.settings_revision,
                rail_revision: n.rail_revision,
                geometry_revision: m.desired_geometry,
                focus_revision: m.desired_focus,
                press: position,
                last_pointer: position,
                initial_offset: observed.offset,
                intended_offset: observed.offset,
                grab_offset: number(grab)?,
                presentation_anchor: number(held.start.get() + held.length.get() * fraction)?,
                travel: held.travel,
                document_rect: rail.document_rect,
            };
            let n = s.navigation.get_mut(&scope.mount).unwrap();
            n.gesture = Some(g);
            n.last_gesture = gesture;
            n.settled = None;
            Ok(())
        })
    }
    pub fn navigation_move(
        &mut self,
        scope: &Scope,
        gesture: u64,
        pointer: u64,
        position: Scalar,
        permission: HostPermission,
        now: u64,
    ) -> Result<Option<RequestId>> {
        self.transaction(|core, s| {
            s.time(now)?;
            s.navigation_scope(core, scope)?;
            s.permission(scope.mount, permission)?;
            let n = s
                .navigation
                .get(&scope.mount)
                .ok_or(Error::Stale("navigation state"))?;
            let g = n
                .gesture
                .as_ref()
                .ok_or(Error::Stale("navigation gesture absent"))?;
            if g.id != gesture || g.pointer != pointer || g.scope != *scope {
                return Err(Error::Stale("navigation gesture identity"));
            }
            if g.geometry_revision != s.mounts[&scope.mount].desired_geometry
                || g.focus_revision != s.mounts[&scope.mount].desired_focus
            {
                return Err(Error::Stale("navigation gesture prerequisites"));
            }
            if position == g.last_pointer {
                return Ok(None);
            }
            let range = n.model.as_ref().unwrap().range()?.get();
            let delta = number(position.get() - g.press.get())?.get();
            let initial = g.initial_offset.get() / range;
            let progress = if delta >= g.travel.get() * (1.0 - initial) {
                1.0
            } else if delta <= -g.travel.get() * initial {
                0.0
            } else {
                number(initial + delta / g.travel.get())?.get()
            };
            let target = number(progress * range)?;
            let request = NavigationRequest {
                model_revision: g.model_revision,
                settings_revision: g.settings_revision,
                rail_revision: g.rail_revision,
                gesture: Some(gesture),
                action: NavigationAction::AbsoluteScroll(target),
            };
            let unchanged = target == g.intended_offset;
            if unchanged {
                s.navigation
                    .get_mut(&scope.mount)
                    .unwrap()
                    .gesture
                    .as_mut()
                    .unwrap()
                    .last_pointer = position;
                return Ok(None);
            }
            let id = s.enqueue(
                core,
                scope.mount,
                RequestKind::Navigation(request),
                permission,
                now,
            )?;
            let n = s.navigation.get_mut(&scope.mount).unwrap();
            let g = n.gesture.as_mut().unwrap();
            g.last_pointer = position;
            g.intended_offset = target;
            n.last_request = Some(id);
            n.last_intent = Some(target);
            Ok(Some(id))
        })
    }
    pub fn navigation_settle(
        &mut self,
        scope: &Scope,
        gesture: u64,
        pointer: u64,
        reason: GestureSettlement,
        now: u64,
    ) -> Result<()> {
        self.transaction(|core, s| {
            s.time(now)?;
            s.navigation_scope(core, scope)?;
            s.settle_navigation(
                scope.mount,
                SettledGesture {
                    gesture,
                    pointer,
                    reason,
                },
            )
        })
    }
    pub fn navigation_key(
        &mut self,
        scope: &Scope,
        key: NavigationKey,
        permission: HostPermission,
        now: u64,
    ) -> Result<RequestId> {
        self.transaction(|core, s| {
            s.time(now)?;
            s.navigation_scope(core, scope)?;
            let n = s
                .navigation
                .get(&scope.mount)
                .ok_or(Error::Stale("navigation state"))?;
            let model = n
                .model
                .as_ref()
                .ok_or(Error::Stale("document model missing"))?;
            let range = model.range()?.get();
            let actual = n.current_observation()?.offset.get();
            let offset = match key {
                NavigationKey::Home => 0.0,
                NavigationKey::End => range,
                NavigationKey::LineBackward => (actual - n.settings.line_step.value.get()).max(0.0),
                NavigationKey::LineForward => {
                    actual + (range - actual).min(n.settings.line_step.value.get())
                }
                NavigationKey::PageBackward => (actual - model.viewport.get()).max(0.0),
                NavigationKey::PageForward => actual + (range - actual).min(model.viewport.get()),
            };
            let r = NavigationRequest {
                model_revision: model.revision,
                settings_revision: n.settings_revision,
                rail_revision: n.rail_revision,
                gesture: None,
                action: NavigationAction::AbsoluteScroll(number(offset)?),
            };
            let id = s.enqueue(
                core,
                scope.mount,
                RequestKind::Navigation(r),
                permission,
                now,
            )?;
            let n = s.navigation.get_mut(&scope.mount).unwrap();
            n.last_request = Some(id);
            n.last_intent = Some(number(offset)?);
            Ok(id)
        })
    }
    pub fn navigation_jump(
        &mut self,
        scope: &Scope,
        marker: &str,
        permission: HostPermission,
        now: u64,
    ) -> Result<RequestId> {
        self.transaction(|core, s| {
            s.time(now)?;
            s.navigation_scope(core, scope)?;
            let n = s
                .navigation
                .get(&scope.mount)
                .ok_or(Error::Stale("navigation state"))?;
            let overview = n
                .overview
                .as_ref()
                .ok_or(Error::Stale("overview missing"))?;
            let target = overview
                .markers
                .iter()
                .find(|m| m.id == marker)
                .and_then(|m| m.jump_target.clone())
                .ok_or(Error::Unsupported("marker jump"))?;
            let r = NavigationRequest {
                model_revision: overview.model_revision,
                settings_revision: n.settings_revision,
                rail_revision: n.rail_revision,
                gesture: None,
                action: NavigationAction::SemanticJump {
                    overview_revision: overview.revision,
                    target,
                },
            };
            let id = s.enqueue(
                core,
                scope.mount,
                RequestKind::Navigation(r),
                permission,
                now,
            )?;
            s.navigation.get_mut(&scope.mount).unwrap().last_request = Some(id);
            Ok(id)
        })
    }
    pub fn navigation_projection(&self, scope: &Scope) -> Result<NavigationProjection> {
        self.state.navigation_scope(&self.core, scope)?;
        let n = self
            .state
            .navigation
            .get(&scope.mount)
            .ok_or(Error::Stale("navigation state"))?;
        let (effective, fallback_reason) =
            n.effective(&self.state.providers[&scope.provider].capabilities);
        let active = n.gesture.is_some();
        let visible = n.settings.visibility.value == RailVisibility::AlwaysVisible
            || n.hover
            || n.focused
            || n.revealed
            || active;
        let offset = match &n.observation {
            Observation::Known(o) => Some(o.offset),
            _ => None,
        };
        let mut thumb = None;
        let mut viewport_rectangle = None;
        if let (Some(model), Some(rail), Some(offset)) = (&n.model, &n.rail, offset) {
            thumb = Some(navigation_thumb(
                model,
                offset,
                rail.origin,
                rail.length,
                n.settings.minimum_thumb.value,
            )?);
            let displayed = n.gesture.as_ref().map_or(offset, |g| g.intended_offset);
            viewport_rectangle = Some(navigation_thumb(
                model,
                displayed,
                rail.map_origin,
                rail.map_length,
                number(f64::MIN_POSITIVE)?,
            )?);
        }
        let held_viewport = active
            && matches!(
                effective,
                Some(NavigationMode::MinimapAlways | NavigationMode::ScrubToMinimap)
            );
        let pointer_mapping = if effective == Some(NavigationMode::MinimapAlways) || held_viewport {
            viewport_rectangle.as_ref()
        } else {
            thumb.as_ref()
        };
        let disabled_reason = if effective.is_none() {
            fallback_reason
        } else if n.model.is_none() {
            Some(NavigationDisabled::ModelMissing)
        } else if offset.is_none() {
            Some(NavigationDisabled::ActualUnknown)
        } else if n.rail.is_none() {
            Some(NavigationDisabled::RailMissing)
        } else if n.model.as_ref().unwrap().range()?.get() == 0.0 {
            Some(NavigationDisabled::NoRange)
        } else if pointer_mapping.is_some_and(|t| !t.pointer_available) {
            Some(NavigationDisabled::NoPointerTravel)
        } else {
            None
        };
        let minimap_visible = matches!(
            effective,
            Some(NavigationMode::MinimapAlways | NavigationMode::ScrollbarAndMinimap)
        ) || effective == Some(NavigationMode::ScrubToMinimap) && active;
        let rail_visible = visible
            && matches!(
                effective,
                Some(
                    NavigationMode::ScrollbarOnly
                        | NavigationMode::ScrollbarAndMinimap
                        | NavigationMode::ScrubToMinimap
                )
            )
            && !(effective == Some(NavigationMode::ScrubToMinimap) && active);
        let request_status = n.last_request.and_then(|id| {
            self.state.get_request(id).map(|p| p.status).or_else(|| {
                self.state
                    .receipts
                    .iter()
                    .find(|r| r.ticket.id == id)
                    .map(|r| r.status)
            })
        });
        Ok(NavigationProjection {
            settings_version: 2,
            selected: n.settings.mode.value,
            effective,
            fallback_reason,
            rail_visible,
            minimap_visible,
            held_viewport,
            gesture: n.gesture.as_ref().map(|g| g.id),
            model_revision: n.model.as_ref().map(|m| m.revision),
            observation: n.observation.clone(),
            requested_offset: n
                .gesture
                .as_ref()
                .map(|g| g.intended_offset)
                .or(n.last_intent),
            thumb,
            viewport_rectangle,
            document_rect: n
                .gesture
                .as_ref()
                .map(|g| g.document_rect)
                .or_else(|| n.rail.as_ref().map(|r| r.document_rect)),
            presentation_anchor: n.gesture.as_ref().map(|g| g.presentation_anchor),
            rail_width: n.settings.rail_width.value,
            thumb_width: n.settings.thumb_width.value,
            hit_width: n.settings.hit_width.value,
            contrast: n.settings.contrast.value,
            orientation: "vertical",
            role: "scrollbar",
            accessible_name: n.settings.accessible_name.value.clone(),
            disabled_reason,
            minimum: number(0.0)?,
            maximum: n.model.as_ref().map(|m| m.range()).transpose()?,
            value: offset,
            transition: n.settings.transition.value,
            last_request: n.last_request,
            request_status,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigationKey {
    Home,
    End,
    LineBackward,
    LineForward,
    PageBackward,
    PageForward,
}
