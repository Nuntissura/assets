// Only meaningful for a worker built without `test-faults` (round unit `quickjs`); empty otherwise.
#![cfg(all(feature = "quickjs", not(feature = "test-faults")))]
//! R5 (SMB-MOTION-JS-WORKER test_hook_in_product): a worker built WITHOUT the `test-faults` feature
//! must ignore the fault-injection environment variable. The worker binary is started directly (hidden,
//! empty environment plus only that variable) and spoken to with hand-encoded protocol frames, so the
//! test does not depend on any host-side option that could set the variable.
use std::{
    io::{Read, Write},
    process::{Command, Stdio},
};

const WORKER: &str = env!("CARGO_BIN_EXE_motion-js-worker");
const FAULT_ENV: &str = "HSK_MOTION_JS_WORKER_TEST_FAULT";

fn frame(payload: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(payload.len()).unwrap().to_le_bytes().to_vec();
    out.extend_from_slice(payload);
    out
}

fn read_frame(reader: &mut impl Read) -> Option<Vec<u8>> {
    let mut prefix = [0_u8; 4];
    reader.read_exact(&mut prefix).ok()?;
    let mut payload = vec![0_u8; usize::try_from(u32::from_le_bytes(prefix)).ok()?];
    reader.read_exact(&mut payload).ok()?;
    Some(payload)
}

#[test]
fn worker_ignores_fault_env_without_test_faults() {
    let mut command = Command::new(WORKER);
    command
        .env_clear()
        .env(FAULT_ENV, "exit_mid_request")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = command.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();

    // HELLO: kind 1, protocol version 1.
    let hello = read_frame(&mut stdout).expect("hello");
    assert_eq!(&hello[..3], &[1, 1, 0]);

    // EVAL (kind 2, version 1, id 9): heap 16 MiB, stack 512 KiB, allowance 1000, 5 s, 16 nodes, no
    // UTC, a source that names the injected exit trigger, empty host-read cache.
    let source = b"var exit_now = 1; exit_now + 6";
    let mut eval = vec![2, 1, 0];
    eval.extend_from_slice(&9_u64.to_le_bytes());
    for value in [16_u64 << 20, 512 << 10, 1000, 5_000_000_000, 16] {
        eval.extend_from_slice(&value.to_le_bytes());
    }
    eval.push(0);
    eval.extend_from_slice(&0_i64.to_le_bytes());
    eval.extend_from_slice(&u32::try_from(source.len()).unwrap().to_le_bytes());
    eval.extend_from_slice(source);
    eval.extend_from_slice(&0_u32.to_le_bytes());
    stdin.write_all(&frame(&eval)).unwrap();
    stdin.flush().unwrap();

    // The fault would exit with code 86 before replying; the product worker evaluates the source:
    // REPORT (kind 4) for id 9, polls, dropped = 1, number tag 0, value 7.
    let reply = read_frame(&mut stdout).expect("a REPORT, not an injected exit");
    assert_eq!(reply[0], 4, "{reply:?}");
    assert_eq!(&reply[3..11], &9_u64.to_le_bytes());
    assert_eq!(&reply[19..21], &[1, 0], "{reply:?}");
    assert_eq!(&reply[21..29], &7.0_f64.to_bits().to_le_bytes());

    drop(stdin);
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(0), "worker exits 0 when stdin closes");
}
