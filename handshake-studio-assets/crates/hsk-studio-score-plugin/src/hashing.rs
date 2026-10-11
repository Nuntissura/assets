//! Streaming SHA-256 of the approved plugin file over ONE fixed buffer (R6 `unbounded_buffer`).
//!
//! Host and worker used to read the whole file into a `Vec` (up to 512 MiB) just to hash it. This
//! module reads it in [`BUFFER_BYTES`] chunks through `sha2` (`=0.10.9`, `force-soft`, pure Rust), so peak
//! memory does not depend on the file size (tests: `tests::large_stream_hashes_in_bounded_memory`,
//! `tests/hash_memory.rs`). `hsk-studio-score` keeps its own streaming hasher private, which is why this
//! crate carries the dependency itself; the result is checked equal to score's one-shot `sha256`
//! (`tests::equals_the_one_shot_digest`).
#![deny(clippy::arithmetic_side_effects)]
use sha2::{Digest, Sha256};
use std::io::{Read, Write};

/// The one buffer a hash uses: 64 KiB, allocated once per call.
pub(crate) const BUFFER_BYTES: usize = 64 << 10;

/// Why a stream could not be hashed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HashError {
    /// A read (or the write of the copy) failed.
    Io,
    /// The stream is longer than the allowed size; nothing past the limit is read.
    TooLarge,
}

/// Reads `reader` to its end in [`BUFFER_BYTES`] chunks, optionally writing every chunk to `tee` in the
/// same order it is hashed (so the copy holds exactly the hashed bytes), and returns the SHA-256 and the
/// number of bytes. A stream longer than `max_bytes` fails with `TooLarge` after at most one more chunk.
pub(crate) fn stream_sha256(
    reader: &mut impl Read,
    mut tee: Option<&mut dyn Write>,
    max_bytes: u64,
) -> Result<([u8; 32], u64), HashError> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; BUFFER_BYTES];
    let mut total: u64 = 0;
    loop {
        let n = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(HashError::Io),
        };
        total = u64::try_from(n)
            .ok()
            .and_then(|n| total.checked_add(n))
            .filter(|total| *total <= max_bytes)
            .ok_or(HashError::TooLarge)?;
        let chunk = buffer.get(..n).ok_or(HashError::Io)?;
        hasher.update(chunk);
        if let Some(copy) = tee.as_mut() {
            copy.write_all(chunk).map_err(|_| HashError::Io)?;
        }
    }
    let mut digest = [0_u8; 32];
    digest.copy_from_slice(&hasher.finalize());
    Ok((digest, total))
}

/// Per-thread allocation meter for tests: a global allocator that records, for the thread that turned it
/// on, the peak number of live bytes above the level at that moment.
#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)] // test instrumentation, not a decoder
pub(crate) mod meter {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    thread_local! {
        static ON: Cell<bool> = const { Cell::new(false) };
        static LIVE: Cell<i64> = const { Cell::new(0) };
        static PEAK: Cell<i64> = const { Cell::new(0) };
    }

    pub(crate) struct Meter;

    // SAFETY: forwards every call to `System` unchanged and only updates thread-local integers.
    #[allow(unsafe_code)]
    unsafe impl GlobalAlloc for Meter {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            // SAFETY: same contract as `System::alloc`.
            let ptr = unsafe { System.alloc(layout) };
            if !ptr.is_null() {
                note(i64::try_from(layout.size()).unwrap_or(i64::MAX));
            }
            ptr
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            note(-i64::try_from(layout.size()).unwrap_or(i64::MAX));
            // SAFETY: same contract as `System::dealloc`.
            unsafe { System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            // SAFETY: same contract as `System::realloc`.
            let moved = unsafe { System.realloc(ptr, layout, new_size) };
            if !moved.is_null() {
                note(
                    i64::try_from(new_size).unwrap_or(i64::MAX)
                        - i64::try_from(layout.size()).unwrap_or(i64::MAX),
                );
            }
            moved
        }
    }

    fn note(delta: i64) {
        // `try_with`: the thread-local may already be gone while the thread is shutting down.
        let _ = ON.try_with(|on| {
            if on.get() {
                let live = LIVE.with(|l| {
                    l.set(l.get().saturating_add(delta));
                    l.get()
                });
                PEAK.with(|p| p.set(p.get().max(live)));
            }
        });
    }

    /// Runs `body` on this thread with metering on and returns its result and the peak bytes it held live
    /// at once (allocations by other threads are not counted).
    pub(crate) fn peak_of<T>(body: impl FnOnce() -> T) -> (T, u64) {
        LIVE.with(|l| l.set(0));
        PEAK.with(|p| p.set(0));
        ON.with(|on| on.set(true));
        let result = body();
        ON.with(|on| on.set(false));
        (result, u64::try_from(PEAK.with(Cell::get)).unwrap_or(0))
    }
}

#[cfg(test)]
#[global_allocator]
static METER: meter::Meter = meter::Meter;

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)] // test code
mod tests {
    use super::*;
    use hsk_studio_score::sha256;
    use std::io::Cursor;

    /// Deterministic non-repeating-looking bytes.
    fn pattern(len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| u8::try_from((i.wrapping_mul(31) ^ (i >> 7)) % 251).unwrap_or(0))
            .collect()
    }

    #[test]
    fn equals_the_one_shot_digest() {
        // Lengths around the 64-byte block and the 64 KiB buffer, plus several buffers.
        for len in [
            0,
            1,
            55,
            56,
            63,
            64,
            65,
            BUFFER_BYTES - 1,
            BUFFER_BYTES,
            BUFFER_BYTES + 1,
            3 * BUFFER_BYTES + 17,
        ] {
            let bytes = pattern(len);
            let (digest, total) = stream_sha256(&mut Cursor::new(&bytes), None, u64::MAX).unwrap();
            assert_eq!(digest, sha256(&bytes), "length {len}");
            assert_eq!(total, u64::try_from(len).unwrap());
        }
        // NIST FIPS 180-4 vector, independent of both implementations.
        let (digest, _) = stream_sha256(&mut Cursor::new(b"abc"), None, 3).unwrap();
        assert_eq!(
            digest,
            [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad
            ]
        );
    }

    #[test]
    fn limit_and_tee_are_exact() {
        let bytes = pattern(2 * BUFFER_BYTES + 5);
        let max = u64::try_from(bytes.len()).unwrap();
        // Exactly at the limit is accepted and the tee receives the same bytes in order.
        let mut copy = Vec::new();
        let (digest, total) =
            stream_sha256(&mut Cursor::new(&bytes), Some(&mut copy), max).unwrap();
        assert_eq!((total, &copy, digest), (max, &bytes, sha256(&bytes)));
        // One byte over is refused without reading on.
        assert_eq!(
            stream_sha256(&mut Cursor::new(&bytes), None, max - 1).err(),
            Some(HashError::TooLarge)
        );
        // A failing copy target is an Io error, not a silent short copy.
        struct Full;
        impl Write for Full {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("full"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        assert_eq!(
            stream_sha256(&mut Cursor::new(&bytes), Some(&mut Full), max).err(),
            Some(HashError::Io)
        );
    }

    /// A reader that produces `len` pattern bytes without ever holding them (no backing buffer).
    struct Generated {
        left: u64,
        at: u64,
    }

    impl Read for Generated {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            let n = usize::try_from(self.left).map_or(out.len(), |left| left.min(out.len()));
            for slot in out.iter_mut().take(n) {
                *slot = u8::try_from(self.at.wrapping_mul(31) % 251).unwrap_or(0);
                self.at = self.at.wrapping_add(1);
            }
            self.left = self.left.saturating_sub(u64::try_from(n).unwrap_or(0));
            Ok(n)
        }
    }

    #[test]
    fn large_stream_hashes_in_bounded_memory() {
        // 96 MiB generated on the fly: the hash must hold about one buffer, not the stream.
        let len: u64 = 96 << 20;
        let (result, peak) =
            meter::peak_of(|| stream_sha256(&mut Generated { left: len, at: 0 }, None, len));
        let (streamed, total) = result.unwrap();
        assert_eq!(total, len);
        assert!(
            peak < 4 << 20,
            "peak {peak} bytes while hashing {len} bytes (buffer is {BUFFER_BYTES})"
        );
        assert!(
            peak >= u64::try_from(BUFFER_BYTES).unwrap(),
            "the meter must see the buffer ({peak})"
        );

        // Oracle: the same bytes hashed in one shot by score's own implementation (the whole stream is
        // materialised OUTSIDE the metered window).
        let mut bytes = vec![0_u8; usize::try_from(len).unwrap()];
        Generated { left: len, at: 0 }
            .read_exact(&mut bytes)
            .unwrap();
        assert_eq!(streamed, sha256(&bytes));
    }
}
