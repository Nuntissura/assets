// Only meaningful for a worker built without `test-faults` (round unit `av1-worker`); empty otherwise.
#![cfg(all(feature = "av1-worker", not(feature = "test-faults")))]
//! R5 (SMB-REEL-NATIVE-AV1-WORKER test_hook_in_product): a worker built WITHOUT the `test-faults`
//! feature must ignore the fault-injection environment variable. The worker binary is started directly
//! (hidden, empty environment plus only that variable) and spoken to with hand-encoded protocol frames,
//! so the test does not depend on any host-side option that could set the variable.
use std::{
    io::{Read, Write},
    process::{Command, Stdio},
};

const WORKER: &str = env!("CARGO_BIN_EXE_reel-native-av1-worker");
const FAULT_ENV: &str = "HSK_REEL_NATIVE_AV1_WORKER_TEST_FAULT";

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
        .env(FAULT_ENV, "exit_on_open")
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

    // OPEN (kind 2, version 1, id 9): max_chunk_bytes 1 MiB, colour untagged, av1C 4:2:0 8-bit.
    let mut open = vec![2, 1, 0];
    open.extend_from_slice(&9_u64.to_le_bytes());
    open.extend_from_slice(&(1_u64 << 20).to_le_bytes());
    open.push(0);
    open.extend_from_slice(&4_u32.to_le_bytes());
    open.extend_from_slice(&[0x81, 0x00, 0x0C, 0x00]);
    stdin.write_all(&frame(&open)).unwrap();
    stdin.flush().unwrap();

    // The fault would exit with code 86 before replying; the product worker answers the request
    // (OPENED kind 5 or a typed ERROR kind 8) for id 9.
    let reply = read_frame(&mut stdout).expect("a reply to OPEN, not an injected exit");
    assert!(matches!(reply[0], 5 | 8), "{reply:?}");
    assert_eq!(&reply[3..11], &9_u64.to_le_bytes());

    drop(stdin);
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(0), "worker exits 0 when stdin closes");
}
