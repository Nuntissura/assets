//! Pure field precedence; hosts retain policy and native recreation ownership.
use crate::{Error, Result};

pub const SETTINGS_VERSION: u16 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingSource {
    Default,
    HostProfile,
    Workspace,
    Surface,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Transition {
    #[default]
    Live,
    Recreate,
    Restart,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved<T> {
    pub value: T,
    pub source: SettingSource,
}
impl<T> Resolved<T> {
    pub fn new(value: T) -> Self {
        Self {
            value,
            source: SettingSource::Default,
        }
    }
    pub fn apply(&mut self, value: &Option<T>, source: SettingSource)
    where
        T: Clone,
    {
        if let Some(value) = value {
            self.value = value.clone();
            self.source = source;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsLayers<P> {
    pub version: u16,
    pub host_profile: P,
    pub workspace: P,
    pub surface: P,
    pub transition: Transition,
}
impl<P: Default> Default for SettingsLayers<P> {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            host_profile: P::default(),
            workspace: P::default(),
            surface: P::default(),
            transition: Transition::Live,
        }
    }
}
impl<P> SettingsLayers<P> {
    pub fn validate_version(&self) -> Result<()> {
        if self.version == SETTINGS_VERSION {
            Ok(())
        } else {
            Err(Error::Unsupported("settings version"))
        }
    }
    pub fn layers(&self) -> [(&P, SettingSource); 3] {
        [
            (&self.host_profile, SettingSource::HostProfile),
            (&self.workspace, SettingSource::Workspace),
            (&self.surface, SettingSource::Surface),
        ]
    }
}
