//! Host side of the staging copy (see `staging.rs`): one private directory per host, one private
//! subdirectory per spawned worker. The host creates and removes the directories; the worker creates
//! the verified copy inside its own subdirectory. Std only.
//!
//! * The host directory is created exclusively (`create_dir` never reuses an existing path) under the
//!   configured root (default: the host process's temp directory); on Unix with mode `0o700`, on
//!   Windows it inherits the access control of that root (the user's temp directory by default is not
//!   writable by other users; pass `HostOptions::staging_root` to choose another).
//! * A worker's subdirectory is removed when its session ends, after the process was reaped (the
//!   supervisor drops it as the session's keepalive, test:
//!   `foundation_contract::staging_directories_follow_the_worker`); the host directory is removed when
//!   the last clone of the host is dropped.
#![deny(clippy::arithmetic_side_effects)]
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
fn create_private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir(path)
}

/// The host's private staging directory. Removed (with everything left in it) on drop.
pub(crate) struct StageRoot {
    dir: PathBuf,
    next: AtomicU64,
}

impl StageRoot {
    /// Creates a fresh directory under `base`. An existing name is never reused; a few distinct names
    /// are tried before giving up.
    pub(crate) fn create(base: &Path) -> io::Result<Self> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let mut last = io::Error::from(io::ErrorKind::AlreadyExists);
        for attempt in 0_u32..8 {
            let dir = base.join(format!(
                "hsk-score-plugin-stage-{}-{:x}-{attempt}",
                std::process::id(),
                nanos
            ));
            match create_private_dir(&dir) {
                Ok(()) => {
                    return Ok(Self {
                        dir,
                        next: AtomicU64::new(1),
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => last = error,
                Err(error) => return Err(error),
            }
        }
        Err(last)
    }

    /// A new private subdirectory for one worker.
    /// The returned guard keeps the host directory alive, so a worker that outlives its host handle
    /// still finds its directory and the host directory goes away with the last worker.
    pub(crate) fn worker_dir(self: &Arc<Self>) -> io::Result<WorkerStage> {
        let n = self.next.fetch_add(1, Ordering::AcqRel);
        let dir = self.dir.join(format!("w{n}"));
        create_private_dir(&dir)?;
        Ok(WorkerStage {
            dir,
            _root: Arc::clone(self),
        })
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.dir
    }
}

/// Removes a directory tree; a file that is briefly locked (a scanner, an unloading library) gets a
/// few short retries. Best effort: a leftover is not an error.
fn remove_tree(dir: &Path) {
    for attempt in 0_u32..5 {
        if fs::remove_dir_all(dir).is_ok() || !dir.exists() {
            return;
        }
        if attempt < 4 {
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
}

impl Drop for StageRoot {
    fn drop(&mut self) {
        remove_tree(&self.dir);
    }
}

/// One worker's staging directory; removed on drop (after the worker was reaped).
pub(crate) struct WorkerStage {
    dir: PathBuf,
    /// After `dir` is removed (fields drop after `Drop::drop`), the last guard removes the host directory.
    _root: Arc<StageRoot>,
}

impl WorkerStage {
    pub(crate) fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for WorkerStage {
    fn drop(&mut self) {
        remove_tree(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_and_worker_directories_are_fresh_and_removed() {
        let base = std::env::temp_dir();
        let a = Arc::new(StageRoot::create(&base).unwrap());
        let b = StageRoot::create(&base).unwrap();
        assert_ne!(a.path(), b.path(), "two hosts never share a directory");
        let (w1, w2) = (a.worker_dir().unwrap(), a.worker_dir().unwrap());
        assert_ne!(w1.path(), w2.path());
        assert!(w1.path().starts_with(a.path()) && w1.path().is_dir());
        fs::write(w1.path().join("leftover.clap"), b"x").unwrap();
        let (w1_path, root_path) = (w1.path().to_owned(), a.path().to_owned());
        drop(w1);
        assert!(!w1_path.exists(), "a worker directory goes with its guard");
        assert!(w2.path().is_dir());
        drop(w2);
        // The host handle is gone but a live worker guard keeps the directory.
        let w3 = a.worker_dir().unwrap();
        drop(a);
        assert!(
            root_path.exists(),
            "the host directory outlives its handle while a worker guard lives"
        );
        drop(w3);
        assert!(
            !root_path.exists(),
            "the host directory goes with the last guard"
        );
        drop(b);
    }
}
