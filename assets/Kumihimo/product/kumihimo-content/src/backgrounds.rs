//! Declarative background composition and host-observed media work. No decoder runs here.
use crate::routing::State;
use crate::*;
use kumihimo_core::{Core, Scalar};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundKind {
    Image,
    Video,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum BackgroundFit {
    Cover,
    Contain,
    Stretch,
    Native,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundLayer {
    BehindContent,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackgroundCapabilities {
    pub image: bool,
    pub video: bool,
    pub compositing: bool,
    pub fits: BTreeSet<BackgroundFit>,
    pub poster: bool,
    pub audio: bool,
    pub host_contrast: bool,
    pub media_budget: bool,
}
impl Default for BackgroundCapabilities {
    fn default() -> Self {
        Self {
            image: false,
            video: false,
            compositing: false,
            fits: BTreeSet::new(),
            poster: false,
            audio: false,
            host_contrast: false,
            media_budget: false,
        }
    }
}
impl BackgroundCapabilities {
    pub fn intersect(&self, other: &Self) -> Self {
        Self {
            image: self.image && other.image,
            video: self.video && other.video,
            compositing: self.compositing && other.compositing,
            fits: self.fits.intersection(&other.fits).copied().collect(),
            poster: self.poster && other.poster,
            audio: self.audio && other.audio,
            host_contrast: self.host_contrast && other.host_contrast,
            media_budget: self.media_budget && other.media_budget,
        }
    }
    pub fn supports(&self, kind: BackgroundKind) -> bool {
        match kind {
            BackgroundKind::Image => self.image,
            BackgroundKind::Video => self.video,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackgroundCrop {
    pub x: Scalar,
    pub y: Scalar,
    pub width: Scalar,
    pub height: Scalar,
}
impl Default for BackgroundCrop {
    fn default() -> Self {
        Self {
            x: Scalar::new(0.0).unwrap(),
            y: Scalar::new(0.0).unwrap(),
            width: Scalar::new(1.0).unwrap(),
            height: Scalar::new(1.0).unwrap(),
        }
    }
}
impl BackgroundCrop {
    pub fn validate(&self) -> Result<()> {
        let (x, y, w, h) = (
            self.x.get(),
            self.y.get(),
            self.width.get(),
            self.height.get(),
        );
        if x < 0.0
            || y < 0.0
            || w <= 0.0
            || h <= 0.0
            || !((x + w).is_finite() && (y + h).is_finite())
            || x + w > 1.0
            || y + h > 1.0
        {
            return Err(Error::Invalid("normalized background crop"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BackgroundSettingsPatch {
    pub brightness: Option<Scalar>,
    pub opacity: Option<Scalar>,
    pub fit: Option<BackgroundFit>,
    pub crop: Option<BackgroundCrop>,
    pub overlay_opacity: Option<Scalar>,
    pub host_contrast: Option<bool>,
    pub reduced_motion: Option<bool>,
    pub muted: Option<bool>,
    pub poster: Option<Option<String>>,
    pub reference: Option<String>,
    pub style: Option<String>,
}
impl BackgroundSettingsPatch {
    pub fn validate(&self) -> Result<()> {
        if let Some(value) = self.brightness {
            range(value, 2.0, "background brightness")?;
        }
        if let Some(value) = self.opacity {
            range(value, 1.0, "background opacity")?;
        }
        if let Some(value) = self.overlay_opacity {
            range(value, 1.0, "legibility overlay")?;
        }
        if let Some(crop) = &self.crop {
            crop.validate()?;
        }
        for value in [
            self.poster.as_ref().and_then(Option::as_ref),
            self.reference.as_ref(),
            self.style.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            token(value)?;
        }
        Ok(())
    }
}
fn range(value: Scalar, max: f64, cause: &'static str) -> Result<()> {
    if value.get() < 0.0 || value.get() > max {
        Err(Error::Invalid(cause))
    } else {
        Ok(())
    }
}
fn token(value: &str) -> Result<()> {
    if value.len() > 256 {
        Err(Error::Admission {
            dimension: "background field bytes",
            limit: 256,
            attempted: value.len(),
        })
    } else {
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackgroundSettings {
    pub brightness: Resolved<Scalar>,
    pub opacity: Resolved<Scalar>,
    pub fit: Resolved<BackgroundFit>,
    pub crop: Resolved<BackgroundCrop>,
    pub overlay_opacity: Resolved<Scalar>,
    pub host_contrast: Resolved<bool>,
    pub reduced_motion: Resolved<bool>,
    pub muted: Resolved<bool>,
    pub poster: Resolved<Option<String>>,
    pub reference: Resolved<String>,
    pub style: Resolved<String>,
}
impl Default for BackgroundSettings {
    fn default() -> Self {
        fn d<T>(value: T) -> Resolved<T> {
            Resolved {
                value,
                source: SettingSource::Default,
            }
        }
        Self {
            brightness: d(Scalar::new(1.0).unwrap()),
            opacity: d(Scalar::new(1.0).unwrap()),
            fit: d(BackgroundFit::Cover),
            crop: d(BackgroundCrop::default()),
            overlay_opacity: d(Scalar::new(0.35).unwrap()),
            host_contrast: d(false),
            reduced_motion: d(false),
            muted: d(true),
            poster: d(None),
            reference: d(String::new()),
            style: d(String::new()),
        }
    }
}
impl BackgroundSettings {
    fn apply(&mut self, patch: &BackgroundSettingsPatch, source: SettingSource) {
        macro_rules! set {($($name:ident),*)=>{$(if let Some(value)=&patch.$name {self.$name=Resolved {value:value.clone(),source};})*};}
        set!(
            brightness,
            opacity,
            fit,
            crop,
            overlay_opacity,
            host_contrast,
            reduced_motion,
            muted,
            poster,
            reference,
            style
        );
    }
    pub fn retained_bytes(&self) -> Result<usize> {
        self.reference
            .value
            .len()
            .checked_add(self.style.value.len())
            .and_then(|n| n.checked_add(self.poster.value.as_ref().map_or(0, String::len)))
            .ok_or(Error::Exhausted("background bytes"))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackgroundPolicy {
    pub allow: bool,
    pub allow_unconstrained: bool,
    pub allow_audio: bool,
    pub allow_host_contrast: bool,
    pub reduced_motion: bool,
}
impl Default for BackgroundPolicy {
    fn default() -> Self {
        Self {
            allow: true,
            allow_unconstrained: false,
            allow_audio: false,
            allow_host_contrast: false,
            reduced_motion: false,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundFallback {
    Unavailable(&'static str),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedBackgroundSettings {
    pub version: u16,
    pub requested: BackgroundSettings,
    pub effective: Option<BackgroundSettings>,
    pub fallback: Option<BackgroundFallback>,
    pub transition: Transition,
    pub layer: BackgroundLayer,
    pub input_transparent: bool,
}
impl ResolvedBackgroundSettings {
    pub fn retained_bytes(&self) -> Result<usize> {
        self.requested
            .retained_bytes()?
            .checked_add(
                self.effective
                    .as_ref()
                    .map_or(Ok(0), BackgroundSettings::retained_bytes)?,
            )
            .ok_or(Error::Exhausted("background settings bytes"))
    }
}
pub fn resolve_background_settings(
    layers: &SettingsLayers<BackgroundSettingsPatch>,
    policy: BackgroundPolicy,
    caps: &BackgroundCapabilities,
    kind: BackgroundKind,
) -> Result<ResolvedBackgroundSettings> {
    layers.validate_version()?;
    let mut requested = BackgroundSettings::default();
    for (patch, source) in layers.layers() {
        patch.validate()?;
        requested.apply(patch, source);
    }
    let reason = if !policy.allow {
        Some("background denied by host policy")
    } else if !caps.supports(kind) || !caps.compositing || !caps.fits.contains(&requested.fit.value)
    {
        Some("background composition unsupported")
    } else if requested.host_contrast.value && (!policy.allow_host_contrast || !caps.host_contrast)
    {
        Some("host contrast unsupported")
    } else if !requested.muted.value && (!policy.allow_audio || !caps.audio) {
        Some("background audio denied")
    } else if (policy.reduced_motion || requested.reduced_motion.value)
        && kind == BackgroundKind::Video
        && (requested.poster.value.as_ref().is_none_or(String::is_empty) || !caps.poster)
    {
        Some("reduced-motion poster unavailable")
    } else {
        None
    };
    let mut effective = requested.clone();
    if policy.reduced_motion {
        effective.reduced_motion.value = true;
    }
    Ok(ResolvedBackgroundSettings {
        version: 2,
        requested,
        effective: reason.is_none().then_some(effective),
        fallback: reason.map(BackgroundFallback::Unavailable),
        transition: layers.transition,
        layer: BackgroundLayer::BehindContent,
        input_transparent: true,
    })
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaWork {
    Run,
    Throttle,
    Paused,
    Poster,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaCeilings {
    pub fps: Option<u16>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}
impl Default for MediaCeilings {
    fn default() -> Self {
        Self {
            fps: Some(30),
            width: Some(1920),
            height: Some(1080),
        }
    }
}
impl MediaCeilings {
    pub const fn unconstrained() -> Self {
        Self {
            fps: None,
            width: None,
            height: None,
        }
    }
    pub fn validate(&self) -> Result<()> {
        if self.fps.is_some_and(|n| !(1..=240).contains(&n))
            || self.width.is_some_and(|n| !(1..=16384).contains(&n))
            || self.height.is_some_and(|n| !(1..=16384).contains(&n))
        {
            return Err(Error::Invalid("media ceilings"));
        }
        if let (Some(w), Some(h)) = (self.width, self.height) {
            w.checked_mul(h).ok_or(Error::Exhausted("media pixels"))?;
        }
        Ok(())
    }
    fn intersect(self, other: Self) -> Self {
        fn min<T: Ord>(a: Option<T>, b: Option<T>) -> Option<T> {
            match (a, b) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            }
        }
        Self {
            fps: min(self.fps, other.fps),
            width: min(self.width, other.width),
            height: min(self.height, other.height),
        }
    }
    fn allows(self, obs: &MediaObservation) -> bool {
        self.fps.is_none_or(|n| obs.fps <= n)
            && self.width.is_none_or(|n| obs.width <= n)
            && self.height.is_none_or(|n| obs.height <= n)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaPolicy {
    pub work: MediaWork,
    pub ceilings: MediaCeilings,
}
impl Default for MediaPolicy {
    fn default() -> Self {
        Self {
            work: MediaWork::Run,
            ceilings: MediaCeilings::default(),
        }
    }
}
impl MediaPolicy {
    pub fn validate(&self, allow_unconstrained: bool) -> Result<()> {
        self.ceilings.validate()?;
        if !allow_unconstrained
            && (self.ceilings.fps.is_none()
                || self.ceilings.width.is_none()
                || self.ceilings.height.is_none())
        {
            return Err(Error::PermissionDenied);
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaBudgetRequest {
    pub aggregate_revision: u64,
    pub paused: bool,
    pub ceilings: MediaCeilings,
}
impl MediaBudgetRequest {
    pub fn validate(&self) -> Result<()> {
        if self.aggregate_revision == 0 {
            return Err(Error::Invalid("media aggregate revision"));
        }
        self.ceilings.validate()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaObservation {
    pub aggregate_revision: u64,
    pub observation_revision: u64,
    pub paused: bool,
    pub fps: u16,
    pub width: u32,
    pub height: u32,
}
impl MediaObservation {
    pub fn validate(&self) -> Result<()> {
        if self.aggregate_revision == 0
            || self.observation_revision == 0
            || self.fps > 240
            || self.width > 16384
            || self.height > 16384
            || (!self.paused && (self.fps == 0 || self.width == 0 || self.height == 0))
            || ((self.width == 0) != (self.height == 0))
        {
            return Err(Error::Invalid("media observation"));
        }
        self.width
            .checked_mul(self.height)
            .ok_or(Error::Exhausted("observed media pixels"))?;
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackgroundState {
    pub kind: BackgroundKind,
    pub settings: ResolvedBackgroundSettings,
    pub settings_revision: u64,
    pub policy: Option<MediaPolicy>,
    pub policy_revision: u64,
    pub host_policy: BackgroundPolicy,
}
impl BackgroundState {
    pub fn new(kind: BackgroundKind) -> Self {
        Self {
            kind,
            settings: ResolvedBackgroundSettings {
                version: 2,
                requested: BackgroundSettings::default(),
                effective: None,
                fallback: Some(BackgroundFallback::Unavailable("settings not resolved")),
                transition: Transition::Live,
                layer: BackgroundLayer::BehindContent,
                input_transparent: true,
            },
            settings_revision: 0,
            policy: None,
            policy_revision: 0,
            host_policy: BackgroundPolicy::default(),
        }
    }
    pub fn retained_bytes(&self) -> Result<usize> {
        self.settings.retained_bytes()
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaState {
    pub revision: u64,
    pub invalidated: bool,
    pub host_ceilings: MediaCeilings,
    pub requested: Option<MediaBudgetRequest>,
    pub actual: Observation<MediaObservation>,
    pub observation_revision: u64,
    pub last_observation: Option<MediaObservation>,
}
impl Default for MediaState {
    fn default() -> Self {
        Self {
            revision: 0,
            invalidated: true,
            host_ceilings: MediaCeilings::unconstrained(),
            requested: None,
            actual: Observation::Unknown,
            observation_revision: 0,
            last_observation: None,
        }
    }
}

impl State {
    pub(crate) fn background_bound(&self, core: &Core, scope: &Scope) -> Result<()> {
        let mount = self
            .mounts
            .get(&scope.mount)
            .ok_or(Error::Missing("mount", scope.mount.0))?;
        if mount.scope != *scope
            || !mount.active
            || !self.binding_valid(core.snapshot(), scope)
            || !self.backgrounds.contains_key(&scope.mount)
        {
            return Err(Error::Stale("background binding"));
        }
        Ok(())
    }
    pub(crate) fn media_changed(&mut self, instance: InstanceId) -> Result<()> {
        let revision = self.id()?;
        let media = self.media.entry(instance).or_default();
        media.revision = revision;
        media.invalidated = false;
        media.actual = Observation::Unknown;
        media.requested = None;
        self.stale_where(|p| {
            p.request.ticket.scope.instance == instance
                && matches!(p.request.kind, RequestKind::MediaBudget(_))
        });
        Ok(())
    }
    pub(crate) fn media_unknown(&mut self, instance: InstanceId) {
        if let Some(media) = self.media.get_mut(&instance) {
            media.actual = Observation::Unknown;
        }
    }
    pub(crate) fn media_invalidate(&mut self, instance: InstanceId) {
        self.media_unknown(instance);
        if let Some(media) = self.media.get_mut(&instance) {
            media.invalidated = true;
            media.requested = None;
        }
        self.stale_where(|p| {
            p.request.ticket.scope.instance == instance
                && matches!(p.request.kind, RequestKind::MediaBudget(_))
        });
    }
    pub(crate) fn media_aggregate(&self, instance: InstanceId) -> Result<MediaBudgetRequest> {
        let i = self
            .instances
            .get(&instance)
            .ok_or(Error::Missing("instance", instance.0))?;
        let media = self
            .media
            .get(&instance)
            .ok_or(Error::Missing("media", instance.0))?;
        if media.invalidated {
            return Err(Error::Stale("media aggregate invalidated"));
        }
        let mut ceilings = media.host_ceilings;
        let mut paused = true;
        let mut consumers = 0usize;
        for id in &i.mounts {
            let m = &self.mounts[id];
            if !m.reserved {
                continue;
            }
            consumers += 1;
            let policy = self.backgrounds.get(id).and_then(|b| b.policy);
            if m.eligibility.protected() {
                paused = false;
            }
            match policy {
                Some(MediaPolicy {
                    work: MediaWork::Paused | MediaWork::Poster,
                    ..
                }) => {}
                Some(policy) => {
                    paused = false;
                    ceilings = ceilings.intersect(policy.ceilings);
                }
                None => {
                    paused = false;
                }
            }
        }
        if consumers == 0 {
            paused = false;
        }
        Ok(MediaBudgetRequest {
            aggregate_revision: media.revision,
            paused,
            ceilings,
        })
    }
    pub(crate) fn media_eligible(
        &self,
        mount: MountId,
        request: &MediaBudgetRequest,
    ) -> Result<()> {
        request.validate()?;
        let m = self
            .mounts
            .get(&mount)
            .ok_or(Error::Missing("mount", mount.0))?;
        if !self.backgrounds.contains_key(&mount)
            || !self.instances[&m.instance]
                .capabilities
                .backgrounds
                .media_budget
        {
            return Err(Error::Unsupported("background media policy"));
        }
        if !self.backgrounds[&mount].host_policy.allow {
            return Err(Error::PermissionDenied);
        }
        if self.media_aggregate(m.instance)? != *request {
            return Err(Error::Stale("media aggregate"));
        }
        Ok(())
    }
    pub(crate) fn media_validate_confirmation(
        &self,
        mount: MountId,
        request: &MediaBudgetRequest,
        obs: &MediaObservation,
    ) -> Result<()> {
        self.media_eligible(mount, request)?;
        obs.validate()?;
        if obs.aggregate_revision != request.aggregate_revision
            || obs.paused != request.paused
            || !request.ceilings.allows(obs)
        {
            return Err(Error::Invalid("media applied echo"));
        }
        self.media_validate_observation(self.mounts[&mount].instance, obs)
    }
    fn media_validate_observation(
        &self,
        instance: InstanceId,
        obs: &MediaObservation,
    ) -> Result<()> {
        obs.validate()?;
        let aggregate = self.media_aggregate(instance)?;
        let media = &self.media[&instance];
        if obs.aggregate_revision != aggregate.aggregate_revision {
            return Err(Error::Stale("media observation aggregate"));
        }
        if obs.paused != aggregate.paused || !aggregate.ceilings.allows(obs) {
            return Err(Error::Invalid("media actual exceeds policy"));
        }
        if obs.observation_revision < media.observation_revision {
            return Err(Error::Stale("media observation revision"));
        }
        if obs.observation_revision == media.observation_revision
            && media.last_observation.as_ref() != Some(obs)
        {
            return Err(Error::Invalid("conflicting media observation"));
        }
        Ok(())
    }
    pub(crate) fn media_apply(&mut self, mount: MountId, obs: &MediaObservation) {
        let instance = self.mounts[&mount].instance;
        let media = self.media.get_mut(&instance).unwrap();
        media.actual = Observation::Known(*obs);
        media.observation_revision = obs.observation_revision;
        media.last_observation = Some(*obs);
    }
}
impl Controller {
    pub fn background_settings(
        &mut self,
        scope: Scope,
        layers: SettingsLayers<BackgroundSettingsPatch>,
        policy: BackgroundPolicy,
        now: u64,
    ) -> Result<ResolvedBackgroundSettings> {
        self.transaction(|core, s| {
            s.time(now)?;
            s.background_bound(core, &scope)?;
            let kind = s.backgrounds[&scope.mount].kind;
            let caps = &s.providers[&scope.provider].capabilities.backgrounds;
            let resolved = resolve_background_settings(&layers, policy, caps, kind)?;
            if let Some(current) = s.backgrounds[&scope.mount].policy {
                current.validate(policy.allow_unconstrained)?;
            }
            let revision = s.id()?;
            let policy_changed = s.backgrounds[&scope.mount].host_policy != policy;
            let b = s.backgrounds.get_mut(&scope.mount).unwrap();
            b.settings = resolved.clone();
            b.settings_revision = revision;
            b.host_policy = policy;
            if policy_changed {
                s.media_changed(scope.instance)?;
            }
            Ok(resolved)
        })
    }
    pub fn media_policy(
        &mut self,
        scope: Scope,
        policy: MediaPolicy,
        permission: HostPermission,
        now: u64,
    ) -> Result<RequestId> {
        self.transaction(|core, s| {
            s.time(now)?;
            s.background_bound(core, &scope)?;
            policy.validate(s.backgrounds[&scope.mount].host_policy.allow_unconstrained)?;
            s.permission(scope.mount, permission)?;
            if matches!(policy.work, MediaWork::Poster)
                && (!s.providers[&scope.provider].capabilities.backgrounds.poster
                    || s.backgrounds[&scope.mount]
                        .settings
                        .requested
                        .poster
                        .value
                        .as_ref()
                        .is_none_or(String::is_empty))
            {
                return Err(Error::Unsupported("media poster unavailable"));
            }
            let revision = s.id()?;
            let b = s.backgrounds.get_mut(&scope.mount).unwrap();
            b.policy = Some(policy);
            b.policy_revision = revision;
            s.media_changed(scope.instance)?;
            let aggregate = s.media_aggregate(scope.instance)?;
            let id = s.enqueue(
                core,
                scope.mount,
                RequestKind::MediaBudget(aggregate),
                permission,
                now,
            )?;
            s.media.get_mut(&scope.instance).unwrap().requested = Some(aggregate);
            Ok(id)
        })
    }
    pub fn media_host_ceilings(&mut self, scope: Scope, ceilings: MediaCeilings) -> Result<()> {
        ceilings.validate()?;
        self.transaction(|core, s| {
            s.background_bound(core, &scope)?;
            s.media.entry(scope.instance).or_default().host_ceilings = ceilings;
            s.media_changed(scope.instance)
        })
    }
    pub fn observe_media(&mut self, scope: Scope, observation: MediaObservation) -> Result<()> {
        self.transaction(|core, s| {
            s.background_bound(core, &scope)?;
            s.media_eligible(scope.mount, &s.media_aggregate(scope.instance)?)?;
            s.media_validate_observation(scope.instance, &observation)?;
            s.media_apply(scope.mount, &observation);
            Ok(())
        })
    }
    pub fn media_satisfaction(&self, mount: MountId) -> Result<Observation<bool>> {
        let m = self
            .state
            .mounts
            .get(&mount)
            .ok_or(Error::Missing("mount", mount.0))?;
        let b = self
            .state
            .backgrounds
            .get(&mount)
            .ok_or(Error::Missing("background", mount.0))?;
        let Some(policy) = b.policy else {
            return Ok(Observation::Unknown);
        };
        Ok(match &self.state.media[&m.instance].actual {
            Observation::Unknown => Observation::Unknown,
            Observation::Known(obs) => Observation::Known(
                if matches!(policy.work, MediaWork::Paused | MediaWork::Poster) {
                    !m.eligibility.protected() && obs.paused
                } else {
                    !obs.paused && policy.ceilings.allows(obs)
                },
            ),
        })
    }
}
