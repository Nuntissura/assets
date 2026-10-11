//! Test-only fault injection for the worker (cargo feature `test-faults`, NOT a default feature: no
//! fault code ships in a normal build). Set only through `HostOptions::test_args` for the child the
//! host spawns: `--fault SPEC` with `crash-on-block=N`, `hang-on-block=N`, `garbage-on-block=N`,
//! `oversize-on-block=N`, `short-on-block=N`, `hang-on-probe`, `partial-state`, `no-state` (block
//! numbers count Process requests from 1).
//!
//! A crash exits with [`FAULT_EXIT_CODE`] instead of aborting, so the platform crash reporter (on
//! Windows a possible foreground dialog) never runs.
use crate::types::Completeness;
use crate::wire;
use std::{io::Write, thread, time::Duration};

/// Exit code of an injected crash.
pub const FAULT_EXIT_CODE: i32 = 86;
/// How long an injected hang lasts before the worker leaves by itself (the host kills it first).
const HANG_LIMIT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    CrashOnBlock(u64),
    HangOnBlock(u64),
    GarbageOnBlock(u64),
    OversizeOnBlock(u64),
    ShortOnBlock(u64),
    HangOnProbe,
    PartialState,
    NoState,
}

#[derive(Default)]
pub(crate) struct Faults(Vec<Fault>);

impl Faults {
    /// Parses one `--fault` value; `None` when it is not a known fault.
    pub(crate) fn add(&mut self, spec: &str) -> Option<()> {
        let (name, number) = match spec.split_once('=') {
            Some((name, number)) => (name, Some(number.parse::<u64>().ok()?)),
            None => (spec, None),
        };
        self.0.push(match (name, number) {
            ("crash-on-block", Some(n)) => Fault::CrashOnBlock(n),
            ("hang-on-block", Some(n)) => Fault::HangOnBlock(n),
            ("garbage-on-block", Some(n)) => Fault::GarbageOnBlock(n),
            ("oversize-on-block", Some(n)) => Fault::OversizeOnBlock(n),
            ("short-on-block", Some(n)) => Fault::ShortOnBlock(n),
            ("hang-on-probe", None) => Fault::HangOnProbe,
            ("partial-state", None) => Fault::PartialState,
            ("no-state", None) => Fault::NoState,
            _ => return None,
        });
        Some(())
    }

    fn has(&self, fault: Fault) -> bool {
        self.0.contains(&fault)
    }

    pub(crate) fn completeness(&self) -> Completeness {
        if self.has(Fault::NoState) {
            Completeness::Unavailable
        } else if self.has(Fault::PartialState) {
            Completeness::Partial
        } else {
            Completeness::Complete
        }
    }

    /// Hangs (then leaves) when the probe fault is configured.
    pub(crate) fn on_probe(&self) {
        if self.has(Fault::HangOnProbe) {
            thread::sleep(HANG_LIMIT);
            std::process::exit(4);
        }
    }

    /// Injects the fault selected for Process request `n`. Returns true when the request must not be
    /// answered normally (the fault already wrote its own bytes).
    pub(crate) fn on_block(&self, n: u64, out: &mut impl Write) -> bool {
        if self.has(Fault::CrashOnBlock(n)) {
            std::process::exit(FAULT_EXIT_CODE);
        }
        if self.has(Fault::HangOnBlock(n)) {
            thread::sleep(HANG_LIMIT);
            std::process::exit(4);
        }
        let mut raw = |bytes: &[u8]| {
            let _ = out.write_all(bytes).and_then(|()| out.flush());
        };
        if self.has(Fault::GarbageOnBlock(n)) {
            raw(&[0xAB; 16]);
            return true;
        }
        if self.has(Fault::OversizeOnBlock(n)) {
            let mut header = Vec::with_capacity(wire::HEADER_BYTES);
            header.extend_from_slice(&wire::MAGIC.to_le_bytes());
            header.extend_from_slice(&wire::PROTOCOL_VERSION.to_le_bytes());
            header.push(wire::K_PROCESSED);
            header.extend_from_slice(&0x7FFF_FFFF_u32.to_le_bytes());
            raw(&header);
            return true;
        }
        if self.has(Fault::ShortOnBlock(n)) {
            let mut partial = Vec::with_capacity(wire::HEADER_BYTES + 10);
            partial.extend_from_slice(&wire::MAGIC.to_le_bytes());
            partial.extend_from_slice(&wire::PROTOCOL_VERSION.to_le_bytes());
            partial.push(wire::K_PROCESSED);
            partial.extend_from_slice(&100_u32.to_le_bytes());
            partial.extend_from_slice(&[0; 10]);
            raw(&partial);
            std::process::exit(0);
        }
        false
    }
}
