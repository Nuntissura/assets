use crate::{content_identity, Device, Engine, Error, ResolvedProfile, Sampling};
use std::{collections::BTreeMap, ffi::OsString, fmt, path::PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeCapability { pub engine: Engine, pub device: Device }

/// Explicit host inputs, not discovered or verified against the machine by this crate.
#[derive(Clone)]
pub struct HostBinding {
    pub executable: PathBuf,
    pub cwd: PathBuf,
    pub data: PathBuf,
    pub model_selector: String,
    pub content_sha256: String,
    pub capabilities: Vec<RuntimeCapability>,
    /// Executor must clear inherited environment before applying this exact map.
    pub environment: BTreeMap<OsString, OsString>,
}

impl fmt::Debug for HostBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostBinding").field("inputs", &"[redacted]").finish()
    }
}

#[derive(Clone)]
pub struct LaunchPlan {
    resolved: ResolvedProfile,
    binding: HostBinding,
    arguments: Vec<OsString>,
}

impl fmt::Debug for LaunchPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LaunchPlan").field("profile_fingerprint", &self.resolved.fingerprint())
            .field("arguments", &"[redacted]").field("environment", &"[redacted]").finish()
    }
}

impl LaunchPlan {
    pub fn resolved(&self) -> &ResolvedProfile { &self.resolved }
    pub fn binding(&self) -> &HostBinding { &self.binding }
    /// Pass directly to Command::args; never join into shell text.
    pub fn arguments(&self) -> &[OsString] { &self.arguments }
}

impl ResolvedProfile {
    pub fn plan(&self, binding: HostBinding, prompt: &str) -> Result<LaunchPlan, Error> {
        let profile = self.profile();
        if [&binding.executable, &binding.cwd, &binding.data].iter().any(|path| !path.is_absolute() || path.to_string_lossy().contains('\0'))
            || binding.environment.iter().any(|(key, value)| key.is_empty() || key.to_string_lossy().contains('=') || key.to_string_lossy().contains('\0') || value.to_string_lossy().contains('\0')) {
            return Err(Error::InvalidBinding);
        }
        if !content_identity(&binding.content_sha256) || binding.content_sha256 != profile.model.content_sha256 {
            return Err(Error::InvalidContentIdentity);
        }
        if !binding.capabilities.contains(&RuntimeCapability { engine: profile.engine, device: profile.device }) {
            return Err(Error::UnsupportedCapability);
        }
        let selector = binding.model_selector.strip_prefix("local/").ok_or(Error::InvalidSelector)?;
        let name = match profile.engine {
            Engine::GenieXQairt => selector,
            Engine::GenieXLlamaCpp => selector.strip_suffix(":Q4_0").ok_or(Error::InvalidSelector)?,
        };
        if name.is_empty() || name.len() > 256 || name.starts_with('.') || name.contains("..")
            || !name.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte)) {
            return Err(Error::InvalidSelector);
        }
        if prompt.is_empty() || prompt.len() > 65_536 || prompt.contains('\0') { return Err(Error::InvalidPrompt); }
        let mut args = vec![OsString::from("--skip-update"), OsString::from("--data-dir"), binding.data.as_os_str().to_owned()];
        if profile.engine == Engine::GenieXQairt { args.extend(["--log".into(), "info".into()]); }
        args.extend(["infer".into(), binding.model_selector.as_str().into(), "--compute".into(), match profile.device { Device::Cpu => "cpu", Device::Npu => "npu" }.into(), "--power-mode".into(), "balanced".into()]);
        if profile.engine == Engine::GenieXLlamaCpp { args.extend(["--nctx".into(), profile.preset.context_limit.to_string().into()]); }
        args.extend(["--max-tokens".into(), profile.preset.max_tokens.to_string().into(), "--think=false".into()]);
        match &profile.preset.sampling {
            Sampling::Qairt { temperature, seed } => args.extend(["--temperature".into(), temperature.to_string().into(), "--seed".into(), seed.to_string().into()]),
            Sampling::LlamaCpp { temperature, top_k, top_p, min_p } => args.extend(["--temperature".into(), temperature.to_string().into(), "--top-k".into(), top_k.to_string().into(), "--top-p".into(), top_p.to_string().into(), "--min-p".into(), min_p.to_string().into()]),
        }
        args.extend(["--prompt".into(), prompt.into()]);
        Ok(LaunchPlan { resolved: self.clone(), binding, arguments: args })
    }
}
