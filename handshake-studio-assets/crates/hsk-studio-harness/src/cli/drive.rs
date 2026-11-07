//! Red stub of the process drive surface (lane SMB-HARNESS-DRIVE-1): the commands exist and refuse.
use super::{CliOutput, Parsed, usage};
use std::io::{BufRead, Write};

pub(super) fn one_shot(command: &str, _parsed: &Parsed) -> CliOutput {
    usage(&format!("`{command}` is not implemented yet"))
}

pub(super) fn session(_parsed: &Parsed, _input: &mut dyn BufRead, output: &mut dyn Write) -> i32 {
    let _ = output.write_all(usage("`--stdin` is not implemented yet").stdout.as_bytes());
    2
}
