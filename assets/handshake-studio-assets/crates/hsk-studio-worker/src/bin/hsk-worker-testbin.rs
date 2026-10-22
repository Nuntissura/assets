//! Test helper for the supervision tests (feature `test-faults` only; never part of a product build).
//! Frames are a u32 LE length followed by the payload. Mode (first argument):
//! `echo` sends HELLO then echoes each frame; `silent` never sends HELLO; `crash7` sends HELLO then exits
//! with code 7 on the first request; `hang` sends HELLO then never answers; `exit9` exits at once.
use std::io::{Read, Write};

fn write_frame(out: &mut impl Write, payload: &[u8]) -> std::io::Result<()> {
    let length = u32::try_from(payload.len()).unwrap_or(u32::MAX);
    out.write_all(&length.to_le_bytes())?;
    out.write_all(payload)?;
    out.flush()
}

fn read_frame(input: &mut impl Read) -> Option<Vec<u8>> {
    let mut prefix = [0_u8; 4];
    input.read_exact(&mut prefix).ok()?;
    let mut payload = vec![0_u8; usize::try_from(u32::from_le_bytes(prefix)).ok()?];
    input.read_exact(&mut payload).ok()?;
    Some(payload)
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    match mode.as_str() {
        "exit9" => std::process::exit(9),
        "silent" => std::thread::sleep(std::time::Duration::from_secs(60)),
        _ => {
            if write_frame(&mut output, b"HI").is_err() {
                return;
            }
            while let Some(frame) = read_frame(&mut input) {
                match mode.as_str() {
                    "crash7" => std::process::exit(7),
                    "hang" => std::thread::sleep(std::time::Duration::from_secs(60)),
                    _ => {
                        if write_frame(&mut output, &frame).is_err() {
                            return;
                        }
                    }
                }
            }
        }
    }
}
