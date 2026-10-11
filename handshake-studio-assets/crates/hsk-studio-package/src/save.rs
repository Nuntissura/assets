//! Copy-first crash-safe publication (STU-IO-187): validate the complete package, write a temp
//! file next to the target, `sync_all`, read it back and re-verify, then atomically replace.
//! The destination is never deleted first; on any failure only the temp file is removed and the
//! last-good destination stays byte-identical.
//!
//! Crash recovery: every temp created by [`StdFs`] has an owner marker `<temp>.owner` that the
//! creating process keeps exclusively locked for the duration of the save. The OS drops that
//! lock when the process dies, so [`sweep_stale_temps`] can tell a dead owner (lock free) from a
//! live one (lock held) without guessing from pids, and it only ever deletes a temp that a valid
//! marker names, never any other file.
use crate::{error::PackageError, hashing::check, limits::Limits, read::verify_container};
use hsk_studio_accord::CancellationToken;
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

const MARKER_SUFFIX: &str = ".owner";
const MARKER_MAGIC: &str = "hsk-studio-package-temp 1";
const MARKER_MAX_BYTES: u64 = 1024;
const SCAN_LIMIT: usize = 100_000;
const RENAME_ATTEMPTS: u32 = 10;

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

struct Held {
    temp: PathBuf,
    marker: Option<PathBuf>,
    lock: Option<File>,
}

/// `std::fs` implementation: owner-marked `create_new` temp in the target directory, `sync_all`,
/// rename with exponential backoff on transient Windows sharing/permission errors (antivirus,
/// indexers). Holds the owner lock of every temp it created until `replace` or `remove`.
#[derive(Default)]
pub struct StdFs {
    held: Vec<Held>,
}

impl StdFs {
    pub fn new() -> Self {
        Self::default()
    }

    fn release(&mut self, temp: &Path) {
        if let Some(i) = self.held.iter().position(|h| h.temp == temp) {
            let held = self.held.swap_remove(i);
            drop(held.lock);
            if let Some(marker) = held.marker {
                let _ = fs::remove_file(marker);
            }
        }
    }
}

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

/// Creates the owner marker and takes its exclusive lock before any content is written, so a
/// concurrent sweep can only ever see a locked marker or an incomplete (ignored) one.
/// `Ok(None)`: the filesystem has no locks, so no marker exists and the temp is never swept.
fn create_marker(marker: &Path, temp_name: &str) -> io::Result<Option<File>> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(marker)?;
    let outcome = file.lock().and_then(|()| {
        let body = format!(
            "{MARKER_MAGIC}\npid {}\ntemp {temp_name}\n",
            std::process::id()
        );
        file.write_all(body.as_bytes())?;
        file.sync_all()
    });
    match outcome {
        Ok(()) => Ok(Some(file)),
        Err(error) => {
            drop(file);
            let _ = fs::remove_file(marker);
            if error.kind() == io::ErrorKind::Unsupported {
                Ok(None)
            } else {
                Err(error)
            }
        }
    }
}

impl AtomicFs for StdFs {
    fn write_temp(&mut self, target: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
        let dir = parent_dir(target);
        let name = target.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "target has no file name")
        })?;
        for attempt in 0..64u32 {
            let mut temp_name = name.to_os_string();
            temp_name.push(format!(".tmp-{}-{attempt}", std::process::id()));
            let temp_text = temp_name.to_string_lossy().into_owned();
            let temp = dir.join(&temp_name);
            let marker = dir.join(format!("{temp_text}{MARKER_SUFFIX}"));
            let lock = match create_marker(&marker, &temp_text) {
                Ok(lock) => lock,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            };
            let marker = lock.as_ref().map(|_| marker);
            let discard = |lock: Option<File>, marker: &Option<PathBuf>| {
                drop(lock);
                if let Some(m) = marker {
                    let _ = fs::remove_file(m);
                }
            };
            match OpenOptions::new().write(true).create_new(true).open(&temp) {
                Ok(mut file) => {
                    let written = file.write_all(bytes).and_then(|()| file.sync_all());
                    drop(file);
                    if let Err(error) = written {
                        let _ = fs::remove_file(&temp);
                        discard(lock, &marker);
                        return Err(error);
                    }
                    self.held.push(Held {
                        temp: temp.clone(),
                        marker,
                        lock,
                    });
                    return Ok(temp);
                }
                Err(error) => {
                    discard(lock, &marker);
                    if error.kind() == io::ErrorKind::AlreadyExists {
                        continue;
                    }
                    return Err(error);
                }
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
        self.release(temp);
        #[cfg(unix)]
        if let Ok(dir) = File::open(parent_dir(target)) {
            let _ = dir.sync_all();
        }
        Ok(())
    }

    fn remove(&mut self, temp: &Path) {
        let _ = fs::remove_file(temp);
        self.release(temp);
    }
}

/// What a [`sweep_stale_temps`] pass did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Temp file names removed (their markers were removed with them).
    pub removed: Vec<String>,
    /// Markers whose owner is alive (lock held, or this very process): left untouched.
    pub skipped_live: u32,
    /// Look-alike or invalid markers/targets: left untouched.
    pub ignored: u32,
}

fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn parse_marker(body: &str, stem: &str) -> Option<u32> {
    let mut lines = body.lines();
    if lines.next()? != MARKER_MAGIC {
        return None;
    }
    let pid = lines.next()?.strip_prefix("pid ")?;
    let temp = lines.next()?.strip_prefix("temp ")?;
    if lines.next().is_some() || temp != stem || !all_digits(pid) {
        return None;
    }
    pid.parse().ok()
}

fn sweep_one(dir: &Path, stem: &str, marker: &Path, report: &mut SweepReport) {
    let is_plain_marker = fs::symlink_metadata(marker)
        .is_ok_and(|m| m.file_type().is_file() && m.len() <= MARKER_MAX_BYTES);
    let opened = OpenOptions::new().read(true).write(true).open(marker);
    let mut file = match opened {
        Ok(file) if is_plain_marker => file,
        _ => {
            report.ignored += 1;
            return;
        }
    };
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            report.skipped_live += 1;
            return;
        }
        Err(TryLockError::Error(_)) => {
            report.ignored += 1;
            return;
        }
    }
    let mut body = String::new();
    let read = (&mut file).take(MARKER_MAX_BYTES).read_to_string(&mut body);
    let Some(pid) = read.ok().and_then(|_| parse_marker(&body, stem)) else {
        report.ignored += 1;
        return;
    };
    if pid == std::process::id() {
        report.skipped_live += 1;
        return;
    }
    let temp = dir.join(stem);
    match fs::symlink_metadata(&temp) {
        Ok(m) if m.file_type().is_file() => {
            if fs::remove_file(&temp).is_err() {
                report.ignored += 1;
                return;
            }
        }
        Ok(_) => {
            report.ignored += 1;
            return;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(_) => {
            report.ignored += 1;
            return;
        }
    }
    drop(file);
    let _ = fs::remove_file(marker);
    report.removed.push(stem.to_owned());
}

/// Removes temp files left by saves to `target` whose owner process is gone.
///
/// Only files named by a valid owner marker (`<target>.tmp-<pid>-<n>.owner` created by [`StdFs`],
/// lock free, naming exactly its own temp) are removed; unmarked files, look-alikes, markers of
/// live owners (exclusive lock held) and the current process are never touched.
pub fn sweep_stale_temps(
    target: &Path,
    cancel: &CancellationToken,
) -> Result<SweepReport, PackageError> {
    check(cancel)?;
    let dir = parent_dir(target);
    let name = target
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or(PackageError::DestinationInvalid)?;
    let prefix = format!("{name}.tmp-");
    let mut report = SweepReport::default();
    let entries = fs::read_dir(dir).map_err(|e| PackageError::Io(e.kind()))?;
    for item in entries.take(SCAN_LIMIT) {
        check(cancel)?;
        let Ok(item) = item else {
            report.ignored += 1;
            continue;
        };
        let file_name = item.file_name();
        let Some(stem) = file_name
            .to_str()
            .and_then(|n| n.strip_suffix(MARKER_SUFFIX))
        else {
            continue;
        };
        let Some((pid, attempt)) = stem
            .strip_prefix(&prefix)
            .and_then(|rest| rest.split_once('-'))
        else {
            continue;
        };
        if !all_digits(pid) || !all_digits(attempt) {
            continue;
        }
        sweep_one(dir, stem, &item.path(), &mut report);
    }
    report.removed.sort();
    Ok(report)
}

/// Saves `package` to `target` with [`StdFs`].
pub fn save_atomic(
    target: &Path,
    package: &[u8],
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<(), PackageError> {
    save_atomic_with(&mut StdFs::new(), target, package, limits, cancel)
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
