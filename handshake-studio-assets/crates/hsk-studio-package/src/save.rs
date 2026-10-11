//! Copy-first crash-safe publication (STU-IO-187): validate the complete package, write a temp
//! file next to the target, `sync_all`, read it back and re-verify, then atomically replace.
//! The destination is never deleted first; on any failure only the temp file is removed and the
//! last-good destination stays byte-identical.
use crate::{
    error::PackageError,
    hashing::check,
    limits::Limits,
    read::verify_container,
};
use hsk_studio_accord::CancellationToken;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

/// Filesystem steps of an atomic save; injectable so every step can be fault-tested.
pub trait AtomicFs {
    /// Creates a fresh temp file beside `target`, writes `bytes`, flushes it to stable storage.
    /// On failure the partial temp file must already be removed.
    fn write_temp(&mut self, target: &Path, bytes: &[u8]) -> io::Result<PathBuf>;
    /// Reads at most `max` bytes of the temp file back.
    fn read_back(&mut self, temp: &Path, max: u64) -> io::Result<Vec<u8>>;
    /// Atomically replaces `target` with `temp`.
    fn replace(&mut self, temp: &Path, target: &Path) -> io::Result<()>;
    /// Best-effort removal of a temp file this save created.
    fn remove(&mut self, temp: &Path);
}

/// `std::fs` implementation: `create_new` temp in the target directory, `sync_all`, rename with
/// exponential backoff on transient Windows sharing/permission errors (antivirus, indexers).
#[derive(Clone, Copy, Debug, Default)]
pub struct StdFs;

const RENAME_ATTEMPTS: u32 = 10;

fn parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

fn is_transient(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::PermissionDenied {
        return true;
    }
    // Windows: ERROR_ACCESS_DENIED 5, ERROR_SHARING_VIOLATION 32, ERROR_LOCK_VIOLATION 33.
    cfg!(windows) && matches!(error.raw_os_error(), Some(5 | 32 | 33))
}

impl AtomicFs for StdFs {
    fn write_temp(&mut self, target: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
        let dir = parent_dir(target);
        let name = target
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "target has no file name"))?;
        for attempt in 0..64u32 {
            let mut temp_name = name.to_os_string();
            temp_name.push(format!(".tmp-{}-{attempt}", std::process::id()));
            let temp = dir.join(temp_name);
            match OpenOptions::new().write(true).create_new(true).open(&temp) {
                Ok(mut file) => {
                    let written = file.write_all(bytes).and_then(|()| file.sync_all());
                    drop(file);
                    if let Err(error) = written {
                        let _ = fs::remove_file(&temp);
                        return Err(error);
                    }
                    return Ok(temp);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "temp names exhausted",
        ))
    }

    fn read_back(&mut self, temp: &Path, max: u64) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        File::open(temp)?
            .take(max.saturating_add(1))
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    fn replace(&mut self, temp: &Path, target: &Path) -> io::Result<()> {
        let mut delay = Duration::from_millis(5);
        let mut attempt = 1;
        loop {
            match fs::rename(temp, target) {
                Ok(()) => break,
                Err(error) if attempt < RENAME_ATTEMPTS && is_transient(&error) => {
                    std::thread::sleep(delay);
                    delay = (delay * 2).min(Duration::from_millis(400));
                    attempt += 1;
                }
                Err(error) => return Err(error),
            }
        }
        #[cfg(unix)]
        if let Ok(dir) = File::open(parent_dir(target)) {
            let _ = dir.sync_all();
        }
        Ok(())
    }

    fn remove(&mut self, temp: &Path) {
        let _ = fs::remove_file(temp);
    }
}

/// Saves `package` to `target` with [`StdFs`].
pub fn save_atomic(
    target: &Path,
    package: &[u8],
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<(), PackageError> {
    save_atomic_with(&mut StdFs, target, package, limits, cancel)
}

/// Saves `package` through an injectable [`AtomicFs`].
///
/// Order: cancel check -> verify the in-memory package -> write+sync temp -> read temp back,
/// verify and compare byte-for-byte -> cancel check -> atomic replace. A pre-cancelled token
/// writes nothing.
pub fn save_atomic_with(
    fs_port: &mut dyn AtomicFs,
    target: &Path,
    package: &[u8],
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<(), PackageError> {
    check(cancel)?;
    if target.is_dir() {
        return Err(PackageError::DestinationInvalid);
    }
    verify_container(package, limits, cancel)?;
    let temp = fs_port
        .write_temp(target, package)
        .map_err(|e| PackageError::Io(e.kind()))?;
    let outcome = publish(fs_port, &temp, target, package, limits, cancel);
    if outcome.is_err() {
        fs_port.remove(&temp);
    }
    outcome
}

fn publish(
    fs_port: &mut dyn AtomicFs,
    temp: &Path,
    target: &Path,
    package: &[u8],
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<(), PackageError> {
    check(cancel)?;
    let read_back = fs_port
        .read_back(temp, package.len() as u64)
        .map_err(|e| PackageError::Io(e.kind()))?;
    verify_container(&read_back, limits, cancel)?;
    if read_back != package {
        return Err(PackageError::ReadbackMismatch);
    }
    check(cancel)?;
    fs_port
        .replace(temp, target)
        .map_err(|e| PackageError::Io(e.kind()))
}
