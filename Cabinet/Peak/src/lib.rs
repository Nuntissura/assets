//! Independent preview state; hosts own authorization, I/O and worker lifetime.
mod decode;
pub use decode::*;
mod scheduler;
pub use scheduler::*;

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Settings {
    pub hover_delay_ms: u64,
    pub expand_delay_ms: u64,
    pub grace_ms: u64,
    pub width: f32,
    pub height: f32,
    pub max_input_bytes: usize,
    pub max_text_lines: usize,
    pub max_output_pixels: u64,
    pub max_decode_pixels: u64,
    pub enabled: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            hover_delay_ms: 450,
            expand_delay_ms: 1200,
            grace_ms: 250,
            width: 460.0,
            height: 340.0,
            max_input_bytes: 128 * 1024 * 1024,
            max_text_lines: 120,
            max_output_pixels: 4_000_000,
            max_decode_pixels: 100_000_000,
            enabled: true,
        }
    }
}
impl Settings {
    pub fn valid(&self) -> bool {
        self.width.is_finite()
            && self.height.is_finite()
            && (100.0..=4096.0).contains(&self.width)
            && (60.0..=4096.0).contains(&self.height)
            && (1..=256 * 1024 * 1024).contains(&self.max_input_bytes)
            && (1..=10000).contains(&self.max_text_lines)
            && (1..=100_000_000).contains(&self.max_output_pixels)
            && (1..=200_000_000).contains(&self.max_decode_pixels)
            && self.hover_delay_ms <= 60000
            && self.expand_delay_ms <= 60000
            && self.grace_ms <= 10000
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Tag {
    pub id: String,
    pub label: String,
    pub color: [u8; 3],
}
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Key {
    pub scope: String,
    pub generation: u64,
    pub context_revision: u64,
    pub entry_id: String,
    pub resource_id: String,
    pub entry_revision: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Content {
    Text {
        body: String,
        truncated: bool,
    },
    Image {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
    Unsupported {
        reason: String,
    },
    Failed {
        reason: String,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Stage {
    Hidden,
    Compact,
    Expanded,
}
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PreviewRequest {
    pub id: u64,
    pub key: Key,
    pub name: String,
    pub max_input_bytes: usize,
    pub max_text_lines: usize,
    pub max_output_pixels: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PreviewCompletion {
    pub request_id: u64,
    pub key: Key,
    pub content: Content,
}
#[derive(Clone, Debug)]
pub struct Peak {
    pub settings: Settings,
    key: Option<Key>,
    name: String,
    tags: Vec<Tag>,
    content: Option<Content>,
    started: u64,
    left: Option<u64>,
    stage: Stage,
    pending: Option<PreviewRequest>,
    sequence: u64,
    dismissed: Option<Key>,
    last_visible: Option<u64>,
}
impl Default for Peak {
    fn default() -> Self {
        Self::new(Settings::default())
    }
}
impl Peak {
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            key: None,
            name: String::new(),
            tags: vec![],
            content: None,
            started: 0,
            left: None,
            stage: Stage::Hidden,
            pending: None,
            sequence: 0,
            dismissed: None,
            last_visible: None,
        }
    }
    pub fn hover(&mut self, key: Key, name: impl Into<String>, tags: Vec<Tag>, now_ms: u64) {
        if !self.settings.enabled || !self.settings.valid() {
            self.dismiss();
            return;
        }
        if self.dismissed.as_ref() == Some(&key) {
            return;
        }
        if self.key.as_ref() != Some(&key) {
            let grace = self
                .last_visible
                .is_some_and(|t| now_ms.saturating_sub(t) <= self.settings.grace_ms);
            self.pending = None;
            self.content = None;
            self.stage = Stage::Hidden;
            self.started = if grace {
                now_ms.saturating_sub(self.settings.hover_delay_ms)
            } else {
                now_ms
            };
            self.key = Some(key);
            self.dismissed = None;
        }
        let name = name.into();
        self.name = if name.len() > 65536 {
            format!("{}… [name truncated]", bounded_string(&name, 65500))
        } else {
            name
        };
        self.tags = tags
            .into_iter()
            .take(64)
            .map(|t| Tag {
                id: bounded_string(&t.id, 256),
                label: bounded_string(&t.label, 256),
                color: t.color,
            })
            .collect();
        self.left = None;
    }
    pub fn leave(&mut self, now_ms: u64) {
        if self.left.is_none() {
            self.left = Some(now_ms);
        }
        self.dismissed = None;
    }
    pub fn dismiss(&mut self) {
        if self.key.is_some() {
            self.dismissed = self.key.clone();
        }
        self.key = None;
        self.pending = None;
        self.content = None;
        self.stage = Stage::Hidden;
        self.name.clear();
        self.tags.clear();
        self.left = None;
    }
    pub fn retain(&mut self) {
        self.left = None;
    }
    pub fn tick(&mut self, now_ms: u64) -> Option<PreviewRequest> {
        if !self.settings.enabled || !self.settings.valid() {
            self.dismiss();
            return None;
        }
        if self
            .left
            .is_some_and(|t| now_ms.saturating_sub(t) > self.settings.grace_ms)
        {
            self.dismiss();
            self.dismissed = None;
            return None;
        }
        let key = self.key.clone()?;
        let elapsed = now_ms.saturating_sub(self.started);
        if elapsed < self.settings.hover_delay_ms {
            return None;
        }
        self.stage = if elapsed
            >= self
                .settings
                .hover_delay_ms
                .saturating_add(self.settings.expand_delay_ms)
        {
            Stage::Expanded
        } else {
            Stage::Compact
        };
        self.last_visible = Some(now_ms);
        if self.stage != Stage::Expanded || self.content.is_some() || self.pending.is_some() {
            return None;
        }
        self.sequence = self.sequence.checked_add(1)?;
        let r = PreviewRequest {
            id: self.sequence,
            key,
            name: self.name.clone(),
            max_input_bytes: self.settings.max_input_bytes,
            max_text_lines: self.settings.max_text_lines,
            max_output_pixels: self.settings.max_output_pixels,
        };
        self.pending = Some(r.clone());
        Some(r)
    }
    pub fn complete(&mut self, completion: PreviewCompletion) -> bool {
        let Some(request) = &self.pending else {
            return false;
        };
        if request.id != completion.request_id
            || request.key != completion.key
            || self.key.as_ref() != Some(&completion.key)
            || !self.settings.enabled
            || !self.settings.valid()
        {
            return false;
        }
        let mut limits = self.settings.clone();
        limits.max_input_bytes = limits.max_input_bytes.min(request.max_input_bytes);
        limits.max_text_lines = limits.max_text_lines.min(request.max_text_lines);
        limits.max_output_pixels = limits.max_output_pixels.min(request.max_output_pixels);
        let content = match validate_content(&completion.content, &limits) {
            Ok(()) => completion.content,
            Err(reason) => Content::Failed { reason },
        };
        self.content = Some(content);
        self.pending = None;
        true
    }
    pub fn stage(&self) -> Stage {
        self.stage
    }
    pub fn key(&self) -> Option<&Key> {
        self.key.as_ref()
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn tags(&self) -> &[Tag] {
        &self.tags
    }
    pub fn content(&self) -> Option<&Content> {
        self.content.as_ref()
    }
    pub fn pending(&self) -> Option<&PreviewRequest> {
        self.pending.as_ref()
    }
}
fn bounded_string(value: &str, max: usize) -> String {
    let mut end = value.len().min(max);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}
pub fn validate_content(content: &Content, s: &Settings) -> Result<(), String> {
    if !s.valid() {
        return Err("Invalid preview settings".into());
    }
    match content {
        Content::Text { body, .. }
            if body.len() > s.max_input_bytes.saturating_mul(2)
                || body.lines().count() > s.max_text_lines =>
        {
            Err("Text preview exceeds configured bounds".into())
        }
        Content::Image {
            width,
            height,
            rgba,
        } => {
            let pixels = u64::from(*width)
                .checked_mul(u64::from(*height))
                .ok_or("Image size overflow")?;
            if pixels == 0
                || pixels > s.max_output_pixels
                || pixels.checked_mul(4) != Some(rgba.len() as u64)
            {
                Err("Image preview exceeds bounds or has invalid RGBA length".into())
            } else {
                Ok(())
            }
        }
        Content::Failed { reason } | Content::Unsupported { reason } if reason.len() > 4096 => {
            Err("Preview diagnostic exceeds bounds".into())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(id: &str) -> Key {
        Key {
            scope: "s".into(),
            generation: 1,
            context_revision: 1,
            entry_id: id.into(),
            resource_id: id.into(),
            entry_revision: 1,
        }
    }
    #[test]
    fn timing_and_late_completion_fence() {
        let mut p = Peak::default();
        p.hover(key("a"), "a", vec![], 0);
        assert!(p.tick(449).is_none());
        assert!(p.tick(450).is_none());
        assert_eq!(p.stage(), Stage::Compact);
        let r = p.tick(1650).unwrap();
        assert_eq!(p.stage(), Stage::Expanded);
        p.hover(key("b"), "b", vec![], 1651);
        assert!(!p.complete(PreviewCompletion {
            request_id: r.id,
            key: r.key,
            content: Content::Text {
                body: "old".into(),
                truncated: false
            }
        }));
    }
    #[test]
    fn dismissal_requires_pointer_leave_to_reopen() {
        let mut p = Peak::default();
        p.hover(key("a"), "a", vec![], 0);
        p.tick(450);
        p.dismiss();
        p.hover(key("a"), "a", vec![], 500);
        assert!(p.tick(1000).is_none());
        p.leave(1001);
        p.hover(key("a"), "a", vec![], 1002);
        assert!(p.tick(2700).is_some());
    }
    #[test]
    fn malformed_image_rejected_as_failure() {
        let mut p = Peak::default();
        p.hover(key("a"), "a", vec![], 0);
        let r = p.tick(1700).unwrap();
        assert!(p.complete(PreviewCompletion {
            request_id: r.id,
            key: r.key,
            content: Content::Image {
                width: 10,
                height: 10,
                rgba: vec![0; 4]
            }
        }));
        assert!(matches!(p.content(), Some(Content::Failed { .. })));
    }
}
