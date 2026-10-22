//! Deterministic domain rules. Hosts own authorization, storage and effects.
pub const DEFAULT_BOARD: &str = "default";
pub const MAX_BOARDS: usize = 64;
pub const MAX_TASKS: usize = 2000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub &'static str);
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.0) }
}
impl std::error::Error for Error {}

pub fn title(value: &str, max_bytes: usize) -> Result<(), Error> {
    if value.trim().is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        return Err(Error("Title must be nonempty, bounded text without control characters"));
    }
    Ok(())
}
pub fn body(value: &str) -> Result<(), Error> {
    if value.len() > 20_000 { return Err(Error("Task body exceeds 20000 bytes")); }
    Ok(())
}
pub fn status(value: &str) -> Result<(), Error> {
    if !matches!(value, "todo" | "in_progress" | "done") { return Err(Error("Invalid task status")); }
    Ok(())
}
pub fn next_revision(current: u64, expected: u64) -> Result<u64, Error> {
    if current != expected { return Err(Error("Task changed; reload before saving")); }
    current.checked_add(1).filter(|v| *v <= i64::MAX as u64).ok_or(Error("Task revision exhausted"))
}
/// Legacy tasks lacking a board stay in their profile's default board. No rewrite.
pub fn board_projection(board: Option<&str>) -> &str { board.unwrap_or(DEFAULT_BOARD) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_stale_and_exhausted_revision_without_wrap() {
        assert!(next_revision(4,3).is_err());
        assert!(next_revision(i64::MAX as u64,i64::MAX as u64).is_err());
        assert_eq!(next_revision(4,4),Ok(5));
    }
    #[test]
    fn rejects_invalid_content_and_preserves_legacy_board() {
        assert!(title(" \n",300).is_err());
        assert!(title("Title",300).is_ok());
        assert!(body(&"a".repeat(20_001)).is_err());
        assert!(status("unknown").is_err());
        assert_eq!(board_projection(None),DEFAULT_BOARD);
        assert_eq!(board_projection(Some("custom")),"custom");
    }
}
