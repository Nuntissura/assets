//! Stable property addressing (STU-MOT-011): validated `/`-joined key segments, never display names
//! or indices.
use crate::error::MotionError;

pub const MAX_PATH_BYTES: usize = 256;
pub const MAX_KEY_BYTES: usize = 128;

fn clean(text: &str) -> bool {
    !text.chars().any(char::is_control)
}

/// Validates one stable key segment: non-empty, no `/`, no control characters, bounded.
pub fn validate_key(key: &str) -> Result<(), MotionError> {
    if key.is_empty()
        || key.len() > MAX_KEY_BYTES
        || key.contains('/')
        || key == "."
        || key == ".."
        || !clean(key)
    {
        Err(MotionError::InvalidKey)
    } else {
        Ok(())
    }
}

/// `composition_id / layer_id / group_key / ... / property_key` joined with `/`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PropertyPath(String);

impl PropertyPath {
    pub fn new(path: &str) -> Result<Self, MotionError> {
        if path.is_empty() || path.len() > MAX_PATH_BYTES || !clean(path) {
            return Err(MotionError::InvalidPath);
        }
        for segment in path.split('/') {
            if segment.is_empty() || segment == "." || segment == ".." {
                return Err(MotionError::InvalidPath);
            }
        }
        Ok(Self(path.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }
}

impl std::fmt::Display for PropertyPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
