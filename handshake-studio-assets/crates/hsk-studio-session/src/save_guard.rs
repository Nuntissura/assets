//! Multi-process save safety (lane SMB-SESSION-WRITE-SAFETY-1, HBR-SWARM-001, GLOBAL-PARALLEL-SAFE): the file boundary
//! of a save. The package crate writes, syncs and re-verifies the temp file; [`GuardedFs`] wraps its filesystem port so
//! the last step, the rename over the destination, happens only after the destination was compared under a lock.
//!
//! **Compare.** A session remembers a [`DiskFingerprint`] of the file it opened or last saved: length plus a digest of
//! its content. Nothing is stored in the package. The fingerprint is read through the handle the session opened, so a
//! replacement after that moment reads as a change. Reading is bounded: a destination whose length differs is refused
//! without reading it, the digest runs in fixed chunks with a [`CancellationToken`] check per chunk, and the read never
//! passes the remembered length plus one byte (test `fingerprint_stops_at_cancel_within_one_chunk`).
//!
//! **Lock.** [`SaveLock`] is a std `File::try_lock` (the OS lock the package crate's owner markers already use on the
//! pinned toolchain, no dependency) on a sidecar beside the destination, [`save_lock_path`], held from the compare
//! until the session has reopened what it landed. The OS drops the lock when the process dies, so
//! a crash leaves at most an unlocked sidecar, which the next save reuses and removes.
//!
//! The sidecar is removed on release, so a process can hold a lock on a file that no longer has the name: it opened
//! the old file, the holder removed it, and a third process created a new one. Acquiring therefore proves the name
//! still points at the locked file: it stamps the locked file (length and modified time, both visible without reading,
//! because a Windows byte-range lock also blocks other handles' reads) and compares the stamp through the name; a
//! mismatch or a missing name drops the handle and tries again (test `a_lock_on_a_removed_sidecar_is_not_a_lock`). On a
//! filesystem that delays the delete, the open fails with permission denied until the holder closes, and that is retried
//! too. Every wait is bounded: past it the save refuses `save_busy`.
//!
//! **What this closes and what it does not.** Between Studio writers using this path the compare and the rename are one
//! step: exactly one save from a revision lands (test `saves_looped_from_processes_with_mid_save_exits_lose_nothing`).
//! A writer that is not Studio takes no lock: if it replaces the file between the compare and the rename, its write is
//! lost silently. Saving to a file `File::try_lock` reports unsupported (no OS locks on that filesystem) runs the compare
//! alone, with the same window between Studio writers. A session reads asset bodies lazily from the file it opened: a
//! Studio save replaces the file by rename, so that handle keeps the old content and every way out stays open, but a
//! writer that overwrites the file in place destroys those bodies, and then saving elsewhere fails typed too unless the
//! document needs none of them.
#![deny(clippy::arithmetic_side_effects)]

use crate::error::SessionError;
use hsk_studio_accord::CancellationToken;
use hsk_studio_package::{AtomicFs, PackageError, StdFs, StreamingFs, sha256_hex_of};
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Fingerprint read size: the only buffer a fingerprint holds.
const CHUNK: usize = 256 * 1024;
/// How long a denied sidecar open is retried: a delete still pending at another process clears in this window, a
/// directory that takes no new file does not.
const DENIED_WAIT: Duration = Duration::from_secs(1);
const FIRST_DELAY: Duration = Duration::from_millis(1);
const MAX_DELAY: Duration = Duration::from_millis(50);

/// Length and content digest of a file on disk. The digest folds SHA-256 over fixed chunks, because the package crate
/// exports only a one-shot SHA-256: it identifies content within a session and is not comparable to `sha256sum`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DiskFingerprint {
    len: u64,
    digest: String,
}

fn fill<R: Read>(reader: &mut R, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while let Some(rest) = buffer.get_mut(filled..).filter(|rest| !rest.is_empty()) {
        match reader.read(rest) {
            Ok(0) => break,
            Ok(n) => filled = filled.saturating_add(n),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

fn fingerprint_reader<R: Read>(reader: &mut R, cancel: &CancellationToken) -> Result<DiskFingerprint, SessionError> {
    let mut buffer = vec![0u8; CHUNK];
    let mut digest = String::new();
    let mut len = 0u64;
    loop {
        cancel.check().map_err(|_| SessionError::Canceled)?;
        let filled = fill(reader, &mut buffer).map_err(|e| SessionError::Io(e.kind()))?;
        let Some(chunk) = buffer.get(..filled).filter(|chunk| !chunk.is_empty()) else {
            return Ok(DiskFingerprint { len, digest });
        };
        len = len.saturating_add(filled as u64);
        digest.push_str(&sha256_hex_of(chunk));
        digest = sha256_hex_of(digest.as_bytes());
    }
}

/// Fingerprints an open file from its start. Used on a clone of the session's source handle: the source seeks before
/// every read, so the shared cursor this moves does not disturb it.
pub(crate) fn fingerprint_file(file: &mut File, cancel: &CancellationToken) -> Result<DiskFingerprint, SessionError> {
    file.seek(SeekFrom::Start(0)).map_err(|e| SessionError::Io(e.kind()))?;
    fingerprint_reader(file, cancel)
}

/// Fingerprints file content a caller already holds in memory (`Session::expect_on_disk`).
pub(crate) fn fingerprint_bytes(bytes: &[u8]) -> Result<DiskFingerprint, SessionError> {
    fingerprint_reader(&mut &*bytes, &CancellationToken::default())
}

fn parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

/// The lock sidecar of a save to `target`: `.hsk-save-lock-<16 hex>` in the same directory, the hex from the lowercased
/// file name (one name per file on a case-insensitive filesystem; a fixed length, so a long name still gets one). It
/// exists only while a save holds it or after a crash during that step, and any later save takes it over.
pub fn save_lock_path(target: &Path) -> PathBuf {
    let name = target.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let hash = sha256_hex_of(name.as_bytes());
    let short = hash.get(..16).unwrap_or(hash.as_str());
    parent_dir(target).join(format!(".hsk-save-lock-{short}"))
}

/// Exclusive while alive. Dropping removes the sidecar first and unlocks after, so nobody can lock this file and find
/// the name still on it once the holder has finished.
pub(crate) struct SaveLock {
    file: File,
    path: PathBuf,
}

impl Drop for SaveLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = self.file.unlock();
    }
}

static STAMPS: AtomicU64 = AtomicU64::new(1);

/// Stamps the locked `file` and says whether `path` still names it (module docs). The length is 1..=65536 from pid,
/// a per-process counter and the clock; the modified time is the clock.
fn names_locked_file(file: &File, path: &Path) -> io::Result<bool> {
    let now = SystemTime::now();
    let nanos = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    let mixed = u64::from(std::process::id())
        .wrapping_mul(31)
        .wrapping_add(STAMPS.fetch_add(1, Ordering::Relaxed).wrapping_mul(131))
        .wrapping_add(u64::from(nanos));
    file.set_len((mixed % 65_536).saturating_add(1))?;
    // A filesystem without settable times leaves the length alone as the stamp.
    let _ = file.set_modified(now);
    let mine = file.metadata()?;
    match fs::metadata(path) {
        Ok(named) => Ok(mine.len() == named.len() && mine.modified().ok() == named.modified().ok()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

impl SaveLock {
    /// `Ok(None)`: the filesystem has no OS locks (module docs). Refusals: `save_busy` past `wait`, `canceled`,
    /// `destination_read_only` when the sidecar cannot be created because the directory takes no new file.
    fn acquire(target: &Path, wait: Duration, cancel: &CancellationToken) -> Result<Option<SaveLock>, SessionError> {
        let path = save_lock_path(target);
        let start = Instant::now();
        let mut delay = FIRST_DELAY;
        loop {
            cancel.check().map_err(|_| SessionError::Canceled)?;
            match OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path) {
                Ok(file) => match file.try_lock() {
                    Ok(()) => match names_locked_file(&file, &path) {
                        Ok(true) => return Ok(Some(SaveLock { file, path })),
                        Ok(false) => {}
                        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {}
                        Err(error) => return Err(SessionError::Io(error.kind())),
                    },
                    Err(TryLockError::WouldBlock) => {}
                    Err(TryLockError::Error(error)) if error.kind() == io::ErrorKind::Unsupported => {
                        drop(file);
                        let _ = fs::remove_file(&path);
                        return Ok(None);
                    }
                    Err(TryLockError::Error(error)) => return Err(SessionError::Io(error.kind())),
                },
                Err(error) if matches!(error.kind(), io::ErrorKind::PermissionDenied | io::ErrorKind::IsADirectory) => {
                    if start.elapsed() >= DENIED_WAIT.min(wait) {
                        return Err(SessionError::DestinationReadOnly);
                    }
                }
                Err(error) => return Err(SessionError::Io(error.kind())),
            }
            if start.elapsed() >= wait {
                return Err(SessionError::SaveBusy);
            }
            std::thread::sleep(delay);
            delay = delay.saturating_mul(2).min(MAX_DELAY);
        }
    }
}

/// What a save expects to find at its destination.
pub(crate) enum Expect {
    /// Whatever is there is replaced: another file the caller named explicitly, or an explicit overwrite.
    Anything,
    /// Nothing may be there.
    Absent,
    /// The file the session opened or last saved, unchanged.
    Unchanged(DiskFingerprint),
}

/// Refusals readable from metadata alone: no content read. Run before the save starts, so a stale session does not
/// stream a whole package first, and again under the lock.
fn check_metadata(expect: &Expect, target: &Path) -> Result<(), SessionError> {
    match fs::metadata(target) {
        Ok(found) if found.is_dir() => Err(SessionError::Package(PackageError::DestinationInvalid)),
        Ok(_) if matches!(expect, Expect::Absent) => Err(SessionError::DestinationExists),
        Ok(found) if found.permissions().readonly() => Err(SessionError::DestinationReadOnly),
        Ok(found) => match expect {
            Expect::Unchanged(fingerprint) if found.len() != fingerprint.len => Err(SessionError::DestinationChanged),
            _ => Ok(()),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound && matches!(expect, Expect::Unchanged(_)) => Err(SessionError::DestinationMissing),
        Err(error) if matches!(expect, Expect::Unchanged(_)) => Err(SessionError::Io(error.kind())),
        Err(_) => Ok(()),
    }
}

/// The full compare. The read stops one byte past the remembered length, so a file that grew after the metadata check
/// cannot make it unbounded.
fn check_content(expect: &Expect, target: &Path, cancel: &CancellationToken) -> Result<(), SessionError> {
    check_metadata(expect, target)?;
    let Expect::Unchanged(fingerprint) = expect else {
        return Ok(());
    };
    let file = File::open(target).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => SessionError::DestinationMissing,
        kind => SessionError::Io(kind),
    })?;
    let found = fingerprint_reader(&mut file.take(fingerprint.len.saturating_add(1)), cancel)?;
    if found == *fingerprint { Ok(()) } else { Err(SessionError::DestinationChanged) }
}

/// [`StdFs`] with the compare and the lock in front of its rename. A refusal is kept in `refusal`, because the package
/// crate's port can only return an `io::Error`; the session reads it back. The lock stays held until this is dropped,
/// which the session does after it has reopened the landed file.
pub(crate) struct GuardedFs<'a> {
    inner: StdFs,
    expect: Expect,
    lock_wait: Duration,
    cancel: &'a CancellationToken,
    lock: Option<SaveLock>,
    pub(crate) landed: Option<DiskFingerprint>,
    pub(crate) refusal: Option<SessionError>,
}

impl<'a> GuardedFs<'a> {
    pub(crate) fn new(expect: Expect, lock_wait: Duration, cancel: &'a CancellationToken) -> Self {
        Self { inner: StdFs::new(), expect, lock_wait, cancel, lock: None, landed: None, refusal: None }
    }

    /// The metadata refusals, before anything is written.
    pub(crate) fn precheck(&self, target: &Path) -> Result<(), SessionError> {
        check_metadata(&self.expect, target)
    }

    /// Why the save failed, if the boundary is the reason: the kept refusal, or, when the writer failed reading assets
    /// from a source another writer has overwritten in place, that the file changed (not the `hash_mismatch` it caused).
    pub(crate) fn refusal_for(&mut self, target: &Path) -> Option<SessionError> {
        self.refusal.take().or_else(|| match check_content(&self.expect, target, self.cancel) {
            Err(error @ (SessionError::DestinationChanged | SessionError::DestinationMissing)) => Some(error),
            _ => None,
        })
    }

    fn refuse(&mut self, error: SessionError) -> io::Error {
        self.refusal = Some(error);
        io::Error::other("save refused at the file boundary")
    }

    /// Lock, compare, fingerprint what lands. Nothing has touched the destination when this fails.
    fn before_rename(&mut self, temp: &Path, target: &Path) -> Result<(), SessionError> {
        self.lock = SaveLock::acquire(target, self.lock_wait, self.cancel)?;
        check_content(&self.expect, target, self.cancel)?;
        let mut file = File::open(temp).map_err(|e| SessionError::Io(e.kind()))?;
        self.landed = Some(fingerprint_file(&mut file, self.cancel)?);
        Ok(())
    }
}

impl AtomicFs for GuardedFs<'_> {
    fn write_temp(&mut self, target: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
        self.inner.write_temp(target, bytes)
    }

    fn read_back(&mut self, temp: &Path, max: u64) -> io::Result<Vec<u8>> {
        self.inner.read_back(temp, max)
    }

    fn replace(&mut self, temp: &Path, target: &Path) -> io::Result<()> {
        if let Err(error) = self.before_rename(temp, target) {
            return Err(self.refuse(error));
        }
        self.inner.replace(temp, target)
    }

    fn remove(&mut self, temp: &Path) {
        self.inner.remove(temp);
    }
}

impl StreamingFs for GuardedFs<'_> {
    fn create_temp(&mut self, target: &Path) -> io::Result<(PathBuf, File)> {
        self.inner.create_temp(target).map_err(|error| {
            if error.kind() == io::ErrorKind::PermissionDenied {
                self.refuse(SessionError::DestinationReadOnly)
            } else {
                error
            }
        })
    }

    fn open_temp(&mut self, temp: &Path) -> io::Result<File> {
        self.inner.open_temp(temp)
    }
}

/// Whether two paths name one destination, also when the file is gone: the parent is resolved and the name compared.
pub(crate) fn same_destination(a: &Path, b: &Path) -> bool {
    fn resolved(path: &Path) -> Option<PathBuf> {
        fs::canonicalize(path).ok().or_else(|| Some(fs::canonicalize(parent_dir(path)).ok()?.join(path.file_name()?)))
    }
    a == b || matches!((resolved(a), resolved(b)), (Some(x), Some(y)) if x == y)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(label: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("hsk-save-guard-{}-{label}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn token() -> CancellationToken {
        CancellationToken::default()
    }

    fn acquire(target: &Path, wait: Duration) -> Result<Option<SaveLock>, SessionError> {
        SaveLock::acquire(target, wait, &token())
    }

    #[test]
    fn a_held_lock_refuses_save_busy_within_the_wait_and_a_released_one_is_gone() {
        let scratch = Scratch::new("busy");
        let target = scratch.0.join("doc.handshake");
        let held = acquire(&target, Duration::from_secs(5)).unwrap().expect("this filesystem has OS locks");
        let start = Instant::now();
        assert!(matches!(acquire(&target, Duration::from_millis(50)), Err(SessionError::SaveBusy)));
        assert!(start.elapsed() < Duration::from_secs(5), "bounded: {:?}", start.elapsed());
        drop(held);
        assert!(!save_lock_path(&target).exists(), "released means removed");
        assert!(acquire(&target, Duration::from_secs(5)).unwrap().is_some());
    }

    /// The race of removing a lock file: B opens the sidecar, the holder removes it and finishes, C creates a new one
    /// and locks it, B's lock on the old file succeeds. B must not count that as holding the lock.
    #[test]
    fn a_lock_on_a_removed_sidecar_is_not_a_lock() {
        let scratch = Scratch::new("orphan");
        let target = scratch.0.join("doc.handshake");
        let path = save_lock_path(&target);
        let holder = acquire(&target, Duration::from_secs(5)).unwrap().expect("OS locks");
        let waiter = OpenOptions::new().read(true).write(true).open(&path).unwrap();
        drop(holder);
        // Nothing has the name: B's handle is on a removed file.
        waiter.try_lock().unwrap();
        assert!(!names_locked_file(&waiter, &path).unwrap(), "the name is gone");
        // C now holds the name; B still holds the removed file's lock.
        let second = acquire(&target, Duration::from_secs(5)).unwrap().expect("C locks the new file, not blocked by B");
        assert!(!names_locked_file(&waiter, &path).unwrap(), "the name is C's file");
        drop(second);
    }

    #[test]
    fn a_crash_leftover_sidecar_is_taken_over_and_removed() {
        let scratch = Scratch::new("leftover");
        let target = scratch.0.join("doc.handshake");
        fs::write(save_lock_path(&target), b"left by a crash").unwrap();
        let held = acquire(&target, Duration::from_secs(5)).unwrap().expect("OS locks");
        drop(held);
        assert!(!save_lock_path(&target).exists());
    }

    /// The sidecar name cannot be created: a directory holds it, the same failure as a directory that takes no new file.
    #[test]
    fn a_sidecar_that_cannot_be_created_refuses_typed() {
        let scratch = Scratch::new("no-sidecar");
        let target = scratch.0.join("doc.handshake");
        fs::create_dir(save_lock_path(&target)).unwrap();
        assert!(matches!(acquire(&target, Duration::from_secs(5)), Err(SessionError::DestinationReadOnly)));
    }

    #[test]
    fn a_long_name_gets_a_short_sidecar_and_two_names_differ() {
        let long = save_lock_path(Path::new(&"n".repeat(300)));
        assert_eq!(long.file_name().unwrap().len(), ".hsk-save-lock-".len() + 16);
        assert_ne!(save_lock_path(Path::new("a.handshake")), save_lock_path(Path::new("b.handshake")));
        assert_eq!(save_lock_path(Path::new("A.handshake")), save_lock_path(Path::new("a.handshake")), "one lock across letter case");
    }

    #[test]
    fn fingerprint_tells_content_and_length_apart() {
        let print = |bytes: &[u8]| fingerprint_reader(&mut &*bytes, &token()).unwrap();
        let body = vec![7u8; CHUNK + 3];
        assert_eq!(print(&body), print(&body));
        assert_eq!(print(&body).len, (CHUNK + 3) as u64);
        let mut other = body.clone();
        other[CHUNK + 1] = 8;
        assert_ne!(print(&body), print(&other), "a change in the last chunk");
        other = body.clone();
        other[0] = 8;
        assert_ne!(print(&body), print(&other), "a change in the first chunk");
        assert_ne!(print(&body), print(&body[..CHUNK]));
        assert_eq!(print(b"").len, 0);
    }

    /// A reader that cancels the token after `after` bytes and counts what is read past it.
    struct CancelAfter<'a> {
        cancel: &'a CancellationToken,
        after: u64,
        read: u64,
    }
    impl Read for CancelAfter<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.read = self.read.saturating_add(buf.len() as u64);
            if self.read >= self.after {
                self.cancel.cancel();
            }
            buf.fill(1);
            Ok(buf.len())
        }
    }

    #[test]
    fn fingerprint_stops_at_cancel_within_one_chunk() {
        let cancel = CancellationToken::default();
        let mut reader = CancelAfter { cancel: &cancel, after: 10 * CHUNK as u64, read: 0 };
        // An endless reader: only the token ends this.
        assert!(matches!(fingerprint_reader(&mut reader, &cancel), Err(SessionError::Canceled)));
        assert!(reader.read <= 11 * CHUNK as u64, "read {}", reader.read);
        let early = CancellationToken::default();
        early.cancel();
        assert!(matches!(fingerprint_reader(&mut &b"bytes"[..], &early), Err(SessionError::Canceled)));
    }

    /// The cost of one fingerprint pass alone. Run by hand: `--run-ignored only`.
    #[test]
    #[ignore = "timing measurement on a generated 512 MiB file; asserts nothing about speed"]
    fn measure_one_fingerprint_pass() {
        const SIZE: usize = 512 << 20;
        let scratch = Scratch::new("measure");
        let path = scratch.0.join("large.bin");
        fs::write(&path, vec![0x5au8; SIZE]).unwrap();
        let mut file = File::open(&path).unwrap();
        let start = Instant::now();
        let print = fingerprint_file(&mut file, &token()).unwrap();
        println!("measured: fingerprint pass over {} bytes {:?}", print.len, start.elapsed());
    }

    /// A destination swapped for a larger file is refused from its length: nothing is read. The proof holds on Windows,
    /// where the exclusive lock makes any read of the file fail, so a read would have answered `io`.
    #[test]
    fn a_compare_reads_nothing_of_a_file_with_another_length() {
        let scratch = Scratch::new("length");
        let target = scratch.0.join("doc.handshake");
        fs::write(&target, b"short").unwrap();
        let expect = Expect::Unchanged(fingerprint_reader(&mut &b"short"[..], &token()).unwrap());
        assert!(check_content(&expect, &target, &token()).is_ok());
        let larger = File::options().write(true).open(&target).unwrap();
        larger.set_len(1 << 20).unwrap();
        larger.lock().unwrap();
        assert!(matches!(check_content(&expect, &target, &token()), Err(SessionError::DestinationChanged)));
    }
}
