//! Worker side: a verified, private staging copy of the approved plugin file (R6 TOCTOU fix).
//!
//! The authorization rule is "foreign code runs only if its SHA-256 equals the approved hash". Hashing
//! a path and then handing the same path to the dynamic loader leaves a window in which the file can be
//! replaced. This module closes it: the worker reads the approved file through ONE open handle, hashes
//! those bytes, writes exactly those bytes into a file it creates exclusively (`create_new`) inside a
//! private directory the host made for this worker, then reads the COPY back through a read handle that
//! it keeps, and hashes it again. The loader is given the copy's path, never the original's.
//!
//! * Windows: the kept handle is opened with `FILE_SHARE_READ` only, so while it is open no other
//!   process can open the copy for writing or deleting (and a pre-existing writer makes the open fail);
//!   the loader itself only needs read sharing. Test: `tests::copy_cannot_be_modified_while_held`.
//! * Unix: the copy is created with mode `0o400` inside a directory the host created with mode `0o700`.
//!   Another process of the same user could still replace the directory contents: NOT_PROVEN against
//!   such a process (it already runs with the plugin's privileges); the original path is out of the
//!   loader's reach in every case (test: `tests::tampering_after_staging_cannot_reach_the_copy`).
//! * Memory: SHA-256 is the one-shot `hsk_studio_score::sha256`, so the file is held in one buffer of at
//!   most the file size (the host charges that size to its admission budget before it spawns the
//!   worker). A streaming hasher over a fixed buffer is NOT_PROVEN/absent until `hsk-studio-score`
//!   exports its streaming `Sha256` (it is `pub(crate)` there).
//! * A plugin that needs sibling files (resources, helper libraries next to the library) does not work
//!   from a lone staged file; that is deliberate, since siblings carry no approved hash.
#![deny(clippy::arithmetic_side_effects)]
use hsk_studio_score::sha256;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

/// Why a file could not be staged. Maps to worker error codes in `backend.rs`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StageError {
    /// Missing, not a regular file, unreadable, or the stage directory refused a create.
    Unavailable,
    /// Larger than the allowed size.
    Budget,
    /// The bytes read (or the copy read back) do not hash to the approved SHA-256.
    Hash,
}

/// The verified private copy. Dropping it closes the kept handle, then removes the copy and the
/// directory made for it (best effort: a copy still mapped by a library cannot be removed until the
/// library is unloaded, so drop the library first; the host removes the whole worker directory after
/// the reap anyway).
pub(crate) struct StagedFile {
    path: PathBuf,
    dir: PathBuf,
    hold: Option<File>,
}

impl StagedFile {
    /// Path of the verified copy: the only path that may be given to the loader.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        drop(self.hold.take());
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_dir(&self.dir);
    }
}

#[cfg(windows)]
fn open_read_held(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_SHARE_READ: others may read (the loader does), nobody may write or delete while we hold it.
    OpenOptions::new().read(true).share_mode(1).open(path)
}

#[cfg(not(windows))]
fn open_read_held(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().read(true).open(path)
}

#[cfg(unix)]
fn create_exclusive(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o400)
        .open(path)
}

#[cfg(not(unix))]
fn create_exclusive(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

/// Reads `source` through one handle (at most `max_bytes`), checks it against `expected`, stages the
/// bytes as `<stage_dir>/s<ordinal>/<file name>` and re-verifies the copy through a kept handle.
/// `ordinal` must differ for every call into the same `stage_dir`; an existing target is an error
/// (`create_dir` and `create_new` never reuse a path).
pub(crate) fn stage_verified(
    source: &Path,
    expected: &[u8; 32],
    max_bytes: u64,
    stage_dir: &Path,
    ordinal: u64,
) -> Result<StagedFile, StageError> {
    let mut original = File::open(source).map_err(|_| StageError::Unavailable)?;
    let meta = original.metadata().map_err(|_| StageError::Unavailable)?;
    if !meta.is_file() {
        return Err(StageError::Unavailable);
    }
    if meta.len() > max_bytes {
        return Err(StageError::Budget);
    }
    let mut bytes =
        Vec::with_capacity(usize::try_from(meta.len()).map_err(|_| StageError::Budget)?);
    (&mut original)
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| StageError::Unavailable)?;
    drop(original);
    if u64::try_from(bytes.len()).map_or(true, |len| len > max_bytes) {
        return Err(StageError::Budget);
    }
    if &sha256(&bytes) != expected {
        return Err(StageError::Hash);
    }

    let name = source
        .file_name()
        .filter(|n| !n.is_empty())
        .ok_or(StageError::Unavailable)?;
    let dir = stage_dir.join(format!("s{ordinal}"));
    fs::create_dir(&dir).map_err(|_| StageError::Unavailable)?;
    let mut staged = StagedFile {
        path: dir.join(name),
        dir,
        hold: None,
    };
    {
        let mut writer = create_exclusive(&staged.path).map_err(|_| StageError::Unavailable)?;
        writer
            .write_all(&bytes)
            .and_then(|()| writer.flush())
            .map_err(|_| StageError::Unavailable)?;
    }
    // Verify the COPY, through the handle that stays open until the loader is done with it.
    let mut hold = open_read_held(&staged.path).map_err(|_| StageError::Unavailable)?;
    bytes.clear();
    (&mut hold)
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| StageError::Unavailable)?;
    if &sha256(&bytes) != expected {
        return Err(StageError::Hash);
    }
    staged.hold = Some(hold);
    Ok(staged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A scratch directory under the test temp dir, removed on drop.
    struct Dir(PathBuf);

    impl Dir {
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "hsk-score-plugin-staging-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::AcqRel)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const PAYLOAD: &[u8] = b"verified plugin bytes";

    fn source(dir: &Dir, bytes: &[u8]) -> PathBuf {
        let path = dir.0.join("plugin.clap");
        fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn tampering_after_staging_cannot_reach_the_copy() {
        let work = Dir::new();
        let stage = work.0.join("stage");
        fs::create_dir(&stage).unwrap();
        let original = source(&work, PAYLOAD);
        let staged = stage_verified(&original, &sha256(PAYLOAD), 1 << 20, &stage, 1).unwrap();

        // The loader gets the copy: another path, inside the private directory, same bytes.
        assert_ne!(staged.path(), original);
        assert!(staged.path().starts_with(&stage));
        assert_eq!(staged.path().file_name(), original.file_name());
        assert_eq!(fs::read(staged.path()).unwrap(), PAYLOAD);

        // The attacker's window (between verification and load): replace or remove the ORIGINAL.
        // The copy the loader will open is unaffected.
        fs::write(&original, b"unapproved code").unwrap();
        assert_eq!(fs::read(staged.path()).unwrap(), PAYLOAD);
        fs::remove_file(&original).unwrap();
        assert_eq!(fs::read(staged.path()).unwrap(), PAYLOAD);
        assert_eq!(sha256(&fs::read(staged.path()).unwrap()), sha256(PAYLOAD));
    }

    #[cfg(windows)]
    #[test]
    fn copy_cannot_be_modified_while_held() {
        let work = Dir::new();
        let stage = work.0.join("stage");
        fs::create_dir(&stage).unwrap();
        let original = source(&work, PAYLOAD);
        let staged = stage_verified(&original, &sha256(PAYLOAD), 1 << 20, &stage, 1).unwrap();
        // A second process-level writer is refused while the verifying handle is kept: write, delete
        // and rename-over all fail with a sharing violation (this process stands in for the attacker;
        // the operating system's share check does not distinguish).
        assert!(OpenOptions::new().write(true).open(staged.path()).is_err());
        assert!(fs::remove_file(staged.path()).is_err());
        assert!(fs::write(staged.path(), b"swap").is_err());
        assert_eq!(fs::read(staged.path()).unwrap(), PAYLOAD);
        // Reading stays allowed (the loader needs it).
        assert!(OpenOptions::new().read(true).open(staged.path()).is_ok());
        let copy = staged.path().to_owned();
        drop(staged);
        assert!(!copy.exists(), "dropping the guard removes the copy");
    }

    #[test]
    fn refusals_leave_nothing_behind() {
        let work = Dir::new();
        let stage = work.0.join("stage");
        fs::create_dir(&stage).unwrap();
        let original = source(&work, PAYLOAD);
        let leftovers = || fs::read_dir(&stage).unwrap().count();

        // Wrong hash: refused before anything is created.
        assert_eq!(
            stage_verified(&original, &sha256(b"other"), 1 << 20, &stage, 1).err(),
            Some(StageError::Hash)
        );
        // Over the size bound: refused before the bytes are read.
        assert_eq!(
            stage_verified(&original, &sha256(PAYLOAD), 4, &stage, 2).err(),
            Some(StageError::Budget)
        );
        // Missing source, and a directory as source.
        assert_eq!(
            stage_verified(&work.0.join("absent"), &sha256(PAYLOAD), 1 << 20, &stage, 3).err(),
            Some(StageError::Unavailable)
        );
        assert_eq!(
            stage_verified(&stage, &sha256(PAYLOAD), 1 << 20, &stage, 4).err(),
            Some(StageError::Unavailable)
        );
        assert_eq!(leftovers(), 0, "no staging directory after a refusal");

        // An ordinal is never reused: the second stage into the same slot is refused, and the first
        // copy is untouched.
        let first = stage_verified(&original, &sha256(PAYLOAD), 1 << 20, &stage, 5).unwrap();
        assert_eq!(
            stage_verified(&original, &sha256(PAYLOAD), 1 << 20, &stage, 5).err(),
            Some(StageError::Unavailable)
        );
        assert_eq!(fs::read(first.path()).unwrap(), PAYLOAD);
        drop(first);
        assert_eq!(leftovers(), 0, "dropping the guard removes its directory");
    }
}
