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
use crate::{
    error::PackageError,
    hashing::check,
    limits::Limits,
    read::{PackageReader, verify_container},
    stream::write_package_streaming,
    write::{AssetStream, Extras, WriteOptions, WriteReceipt},
};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::Snapshot;
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, BufWriter, Read, Write},
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

/// Filesystem steps of a streaming atomic save. Extends [`AtomicFs`], whose `replace` and
/// `remove` are reused unchanged (rename with backoff, owner marker release).
pub trait StreamingFs: AtomicFs {
    /// Creates a fresh temp file beside `target`, opened for writing. The caller writes, syncs and
    /// closes it, then hands the path back to `replace` or `remove`.
    fn create_temp(&mut self, target: &Path) -> io::Result<(PathBuf, File)>;
    /// Opens a temp file this save created for reading (post-write verification).
    fn open_temp(&mut self, temp: &Path) -> io::Result<File>;
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

impl StdFs {
    /// Owner-marked `create_new` temp beside `target`; the owner lock is held (registered in
    /// `held`) until `replace` or `remove`.
    fn create(&mut self, target: &Path) -> io::Result<(PathBuf, File)> {
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
            match OpenOptions::new().write(true).create_new(true).open(&temp) {
                Ok(file) => {
                    self.held.push(Held {
                        temp: temp.clone(),
                        marker,
                        lock,
                    });
                    return Ok((temp, file));
                }
                Err(error) => {
                    drop(lock);
                    if let Some(m) = &marker {
                        let _ = fs::remove_file(m);
                    }
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
}

impl StreamingFs for StdFs {
    fn create_temp(&mut self, target: &Path) -> io::Result<(PathBuf, File)> {
        self.create(target)
    }

    fn open_temp(&mut self, temp: &Path) -> io::Result<File> {
        File::open(temp)
    }
}

impl AtomicFs for StdFs {
    fn write_temp(&mut self, target: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
        let (temp, mut file) = self.create(target)?;
        let written = file.write_all(bytes).and_then(|()| file.sync_all());
        drop(file);
        match written {
            Ok(()) => Ok(temp),
            Err(error) => {
                self.remove(&temp);
                Err(error)
            }
        }
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

/// Write buffer between the streaming writer and the temp file (headers and small entries; large
/// bodies bypass it in 1 MiB chunks).
const STREAM_BUFFER: usize = 256 * 1024;

/// Everything a streaming save writes: the document, the asset stream and the optional extras.
#[derive(Clone, Copy)]
pub struct StreamSave<'a> {
    pub doc: &'a Snapshot,
    pub assets: &'a dyn AssetStream,
    pub extras: Extras<'a>,
    pub options: WriteOptions,
}

/// Streams `save` to `target` with [`StdFs`]: asset bytes flow from the [`AssetStream`] into the
/// temp file in bounded chunks and are SHA-256 verified on the way, so peak memory does not grow
/// with asset size. Same crash-safety contract as [`save_atomic`].
pub fn save_atomic_streaming(
    target: &Path,
    save: &StreamSave<'_>,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<WriteReceipt, PackageError> {
    save_atomic_streaming_with(&mut StdFs::new(), target, save, limits, cancel)
}

/// Streaming atomic save through an injectable [`StreamingFs`].
///
/// Order: cancel check -> create owner-marked temp -> stream the package (digest and length
/// verified per asset; a mismatch aborts here) -> flush + `sync_all` -> reopen the temp and verify
/// structure, every CRC-32, the manifest and every entry's SHA-256 -> cancel check -> atomic
/// replace with backoff. On any failure only the temp (and its marker) is removed and the
/// last-good destination stays byte-identical.
pub fn save_atomic_streaming_with(
    fs_port: &mut dyn StreamingFs,
    target: &Path,
    save: &StreamSave<'_>,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<WriteReceipt, PackageError> {
    check(cancel)?;
    if target.is_dir() {
        return Err(PackageError::DestinationInvalid);
    }
    let (temp, file) = fs_port
        .create_temp(target)
        .map_err(|e| PackageError::Io(e.kind()))?;
    let outcome = stream_and_publish(fs_port, &temp, file, target, save, limits, cancel);
    if outcome.is_err() {
        fs_port.remove(&temp);
    }
    outcome
}

fn stream_and_publish(
    fs_port: &mut dyn StreamingFs,
    temp: &Path,
    file: File,
    target: &Path,
    save: &StreamSave<'_>,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<WriteReceipt, PackageError> {
    let io_kind = |e: io::Error| PackageError::Io(e.kind());
    let mut out = BufWriter::with_capacity(STREAM_BUFFER, file);
    let receipt = write_package_streaming(
        save.doc,
        save.assets,
        &save.extras,
        &save.options,
        limits,
        cancel,
        &mut out,
    )?;
    let file = out.into_inner().map_err(|e| io_kind(e.into_error()))?;
    file.sync_all().map_err(io_kind)?;
    let length = file.metadata().map_err(io_kind)?.len();
    drop(file);
    if length != receipt.bytes_written {
        return Err(PackageError::ReadbackMismatch);
    }
    check(cancel)?;
    // Read-back: the file is re-verified from disk, not from what the writer believes it wrote.
    let reader_file = fs_port.open_temp(temp).map_err(io_kind)?;
    let mut reader = PackageReader::open(reader_file, limits, cancel)?;
    let manifest = reader.manifest();
    if manifest.document_id != save.doc.document().document_id
        || manifest.revision != receipt.doc_revision
    {
        return Err(PackageError::ReadbackMismatch);
    }
    reader.verify_all(cancel)?;
    drop(reader);
    check(cancel)?;
    fs_port.replace(temp, target).map_err(io_kind)?;
    Ok(receipt)
}
