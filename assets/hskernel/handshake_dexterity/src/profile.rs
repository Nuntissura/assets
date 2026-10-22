use crate::{content_identity, identity, Error};
use handshake_canonical_json::{canonical_json_bytes, sha256_hex};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine { GenieXQairt, GenieXLlamaCpp }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Device { Cpu, Npu }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelFormat { QairtW4a16, GgufQ4_0 }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Model {
    pub id: String,
    pub format: ModelFormat,
    /// Host-verified file or bundle content digest; this crate does not read the files.
    pub content_sha256: String,
    pub compiled_context: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Sampling {
    Qairt { temperature: f64, seed: u64 },
    LlamaCpp { temperature: f64, top_k: u32, top_p: f64, min_p: f64 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub name: String,
    pub context_limit: u32,
    pub max_tokens: u32,
    pub sampling: Sampling,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineProfile {
    pub id: String,
    pub revision: u64,
    pub model: Model,
    pub engine: Engine,
    pub preset: Preset,
    pub device: Device,
}

impl EngineProfile {
    pub fn validate(&self) -> Result<(), Error> {
        if !identity(&self.id) || !identity(&self.model.id) || !identity(&self.preset.name) || self.revision == 0 {
            return Err(Error::InvalidIdentity);
        }
        if !content_identity(&self.model.content_sha256) { return Err(Error::InvalidContentIdentity); }
        let preset = &self.preset;
        if preset.context_limit == 0 || preset.max_tokens == 0 || preset.max_tokens > preset.context_limit
            || preset.context_limit > i32::MAX as u32
            || self.model.compiled_context.is_some_and(|limit| limit == 0 || preset.context_limit > limit) {
            return Err(Error::InvalidBounds);
        }
        let temperature = match &preset.sampling {
            Sampling::Qairt { temperature, .. } | Sampling::LlamaCpp { temperature, .. } => *temperature,
        };
        if !temperature.is_finite() || !(0.0..=2.0).contains(&temperature) { return Err(Error::InvalidBounds); }
        match (&self.engine, &self.model.format, &preset.sampling) {
            (Engine::GenieXQairt, ModelFormat::QairtW4a16, Sampling::Qairt { .. })
                if self.device == Device::Npu && self.model.compiled_context.is_some() => {
                    if let Sampling::Qairt { seed, .. } = &preset.sampling {
                        if *seed > i32::MAX as u64 { return Err(Error::InvalidBounds); }
                    }
                }
            (Engine::GenieXLlamaCpp, ModelFormat::GgufQ4_0, Sampling::LlamaCpp { top_k, top_p, min_p, .. }) => {
                if *top_k == 0 || *top_k > i32::MAX as u32 || !top_p.is_finite() || !min_p.is_finite()
                    || !(0.0..=1.0).contains(top_p) || !(0.0..=1.0).contains(min_p) {
                    return Err(Error::InvalidBounds);
                }
            }
            _ => return Err(Error::IncompatibleProfile),
        }
        Ok(())
    }
}

/// An owned validated snapshot. Later changes to caller-owned profile values cannot alter it.
#[derive(Clone, Debug)]
pub struct ResolvedProfile {
    profile: EngineProfile,
    fingerprint: String,
}

impl ResolvedProfile {
    pub fn profile(&self) -> &EngineProfile { &self.profile }
    pub fn fingerprint(&self) -> &str { &self.fingerprint }
}

#[derive(Clone, Debug)]
pub struct Registry { profiles: BTreeMap<String, ResolvedProfile> }

impl Registry {
    pub fn new(profiles: Vec<EngineProfile>) -> Result<Self, Error> {
        let mut entries = BTreeMap::new();
        for profile in profiles {
            profile.validate()?;
            let value = serde_json::to_value(&profile).map_err(|_| Error::InvalidWire)?;
            let fingerprint = sha256_hex(&canonical_json_bytes(&value));
            if entries.insert(profile.id.clone(), ResolvedProfile { profile, fingerprint }).is_some() {
                return Err(Error::DuplicateProfile);
            }
        }
        Ok(Self { profiles: entries })
    }

    /// Strict bounded import; all profile validation still runs after deserialization.
    pub fn from_json(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > 1_048_576 { return Err(Error::InvalidWire); }
        Self::new(serde_json::from_slice(bytes).map_err(|_| Error::InvalidWire)?)
    }

    pub fn resolve(&self, id: &str) -> Result<ResolvedProfile, Error> {
        self.profiles.get(id).cloned().ok_or(Error::MissingProfile)
    }

    /// Enumerate the same validated snapshots in ascending profile-ID order.
    pub fn profiles(&self) -> impl ExactSizeIterator<Item = &ResolvedProfile> {
        self.profiles.values()
    }
}
