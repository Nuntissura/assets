//! Command-line surface as a pure function: arguments in, (exit code, stdout text) out. The
//! example binary only forwards process arguments and prints the result, so every command is
//! testable without spawning a process, opening a window or touching the environment.
//!
//! Two refusals keep a green exit honest (tests: `verify_checks_something_and_save_never_clobbers_the_input`
//! in `tests/harness_headless.rs`): `verify` with no expectation is a usage error (exit 2), and
//! `edit --save OUT` where OUT is the input PACKAGE is a usage error unless `--overwrite` is given.
#![deny(clippy::arithmetic_side_effects)]

use crate::harness::{Harness, HarnessError, References, Selection, default_actor};
use crate::manual::{manual_json, manual_text};
use hsk_studio_accord::CancellationToken;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub struct CliOutput {
    pub exit: i32,
    pub stdout: String,
}

enum EditOp {
    Rename(String, String),
    Visible(String, bool),
    Set(String, String, String),
}

#[derive(Default)]
struct Parsed {
    positional: Vec<String>,
    providers: Vec<String>,
    references: References,
    layer: Option<String>,
    expect_revision: Option<u64>,
    save: Option<PathBuf>,
    overwrite: bool,
    cancel: bool,
    json: bool,
    edits: Vec<EditOp>,
    expect_names: Vec<(String, String)>,
    expect_visible: Vec<(String, bool)>,
}

fn boolean(s: &str) -> Result<bool, String> {
    match s {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(format!("expected true or false, got `{other}`")),
    }
}

fn parse(args: &[String]) -> Result<Parsed, String> {
    let mut p = Parsed::default();
    let mut i = 0;
    let take = |i: &mut usize, what: &str| -> Result<String, String> {
        let v = args.get(*i).ok_or_else(|| format!("missing {what}"))?;
        *i = i.saturating_add(1);
        Ok(v.clone())
    };
    while let Some(arg) = args.get(i).cloned() {
        i = i.saturating_add(1);
        match arg.as_str() {
            "--provider" => p.providers.push(take(&mut i, "provider id")?),
            "--references" => {
                p.references = match take(&mut i, "references mode")?.as_str() {
                    "unresolved" => References::Unresolved,
                    "trust_all" => References::TrustAll,
                    other => return Err(format!("unknown references mode `{other}`")),
                }
            }
            "--layer" => p.layer = Some(take(&mut i, "layer id")?),
            "--expect-revision" => {
                p.expect_revision =
                    Some(take(&mut i, "revision")?.parse::<u64>().map_err(|_| "revision must be an unsigned integer".to_string())?)
            }
            "--save" => p.save = Some(PathBuf::from(take(&mut i, "output path")?)),
            "--overwrite" => p.overwrite = true,
            "--cancel" => p.cancel = true,
            "--json" => p.json = true,
            "--rename" => {
                let layer = take(&mut i, "layer id")?;
                p.edits.push(EditOp::Rename(layer, take(&mut i, "name")?));
            }
            "--visible" => {
                let layer = take(&mut i, "layer id")?;
                p.edits.push(EditOp::Visible(layer, boolean(&take(&mut i, "true|false")?)?));
            }
            "--set" => {
                let layer = take(&mut i, "layer id")?;
                let control = take(&mut i, "control id")?;
                p.edits.push(EditOp::Set(layer, control, take(&mut i, "value")?));
            }
            "--expect-name" => {
                let layer = take(&mut i, "layer id")?;
                p.expect_names.push((layer, take(&mut i, "name")?));
            }
            "--expect-visible" => {
                let layer = take(&mut i, "layer id")?;
                p.expect_visible.push((layer, boolean(&take(&mut i, "true|false")?)?));
            }
            flag if flag.starts_with("--") => return Err(format!("unknown flag `{flag}`")),
            _ => p.positional.push(arg),
        }
    }
    Ok(p)
}

fn out(exit: i32, value: &Value) -> CliOutput {
    CliOutput { exit, stdout: format!("{}\n", serde_json::to_string_pretty(value).unwrap_or_default()) }
}

fn usage(message: &str) -> CliOutput {
    out(2, &json!({"schema":crate::SCHEMA,"ok":false,"error":"usage","message":message,"hint":"run `help`"}))
}

fn refused(command: &str, error: &HarnessError) -> CliOutput {
    out(1, &json!({"schema":crate::SCHEMA,"command":command,"ok":false,"verdict":"fail","error":error.code(),"detail":error.to_string()}))
}

/// Runs one command line (without the program name).
pub fn run(args: &[String]) -> CliOutput {
    let Some((command, rest)) = args.split_first() else {
        return help(&Parsed::default());
    };
    let command = command.as_str();
    if matches!(command, "--help" | "-h") {
        return help(&Parsed::default());
    }
    let parsed = match parse(rest) {
        Ok(p) => p,
        Err(m) => return usage(&m),
    };
    match command {
        "help" => help(&parsed),
        "descriptors" | "providers" => with_harness(command, &parsed, |h| {
            if command == "descriptors" {
                match h.collect_descriptors() {
                    Ok(d) => out(0, &json!({"schema":crate::SCHEMA,"command":command,"ok":true,"descriptors":d})),
                    Err(e) => refused(command, &e),
                }
            } else {
                out(
                    0,
                    &json!({"schema":crate::SCHEMA,"command":command,"ok":true,"available":crate::AVAILABLE_PROVIDERS,"selected":h.selection().providers}),
                )
            }
        }),
        "open" | "panel" | "edit" | "verify" => {
            let Some(package) = parsed.positional.first().map(PathBuf::from) else {
                return usage("missing PACKAGE path");
            };
            if parsed.positional.len() > 1 {
                return usage("only one PACKAGE path is accepted");
            }
            with_harness(command, &parsed, |h| execute(command, h, &parsed, &package))
        }
        other => usage(&format!("unknown command `{other}`")),
    }
}

fn with_harness(command: &str, parsed: &Parsed, f: impl FnOnce(&Harness) -> CliOutput) -> CliOutput {
    match Harness::new(Selection { providers: parsed.providers.clone(), references: parsed.references }) {
        Ok(h) => f(&h),
        Err(e) => refused(command, &e),
    }
}

fn help(parsed: &Parsed) -> CliOutput {
    with_harness("help", parsed, |h| {
        if parsed.json {
            out(0, &manual_json(h))
        } else {
            CliOutput { exit: 0, stdout: manual_text(h) }
        }
    })
}

/// Whether two paths name the same file (resolved when both exist, literal comparison otherwise).
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn execute(command: &str, harness: &Harness, parsed: &Parsed, package: &Path) -> CliOutput {
    // A verify that checks nothing would report a pass for nothing: refuse it before opening.
    if command == "verify" && parsed.expect_revision.is_none() && parsed.expect_names.is_empty() && parsed.expect_visible.is_empty() {
        return usage("verify needs at least one of --expect-revision, --expect-name, --expect-visible (a verify that checks nothing is refused)");
    }
    // The input package is replaced in place only on explicit request.
    if let Some(save) = &parsed.save
        && command == "edit"
        && !parsed.overwrite
        && same_file(save, package)
    {
        return usage("--save names the input PACKAGE; pass --overwrite to replace it in place, or save to another path");
    }
    let cancel = CancellationToken::default();
    if parsed.cancel {
        cancel.cancel();
    }
    let mut run = harness.run(default_actor(), &cancel);
    let opened = run.open_path(package);
    match command {
        "open" => {
            if opened {
                run.summary();
            }
        }
        "panel" => match &parsed.layer {
            None => return usage("panel needs --layer ID"),
            Some(layer) => {
                if opened {
                    run.panel(layer);
                }
            }
        },
        "verify" => {
            if opened {
                let mut expectations: Vec<(String, &'static str, Value)> = Vec::new();
                expectations.extend(parsed.expect_names.iter().map(|(l, n)| (l.clone(), "name", json!(n))));
                expectations.extend(parsed.expect_visible.iter().map(|(l, v)| (l.clone(), "visible", json!(v))));
                run.verify_live(parsed.expect_revision, expectations);
            }
        }
        _ => {
            if parsed.edits.is_empty() {
                return usage("edit needs at least one of --rename, --visible, --set");
            }
            if opened {
                run.summary();
                edit_flow(&mut run, parsed);
            }
        }
    }
    let report = run.finish(command);
    out(i32::from(report["ok"] != true), &report)
}

fn edit_flow(run: &mut crate::Run<'_>, parsed: &Parsed) {
    let expect = parsed.expect_revision;
    for edit in &parsed.edits {
        let ok = match edit {
            EditOp::Rename(layer, name) => run.rename(layer, name, expect),
            EditOp::Visible(layer, visible) => run.set_visible(layer, *visible, expect),
            EditOp::Set(layer, control, value) => run.panel_set(layer, control, value, expect),
        };
        if !ok {
            return;
        }
    }
    let reopened = match &parsed.save {
        Some(path) => run.save_to(path) && run.reopen_path(path),
        None => match run.checkpoint() {
            Some(bytes) => run.reopen_bytes(&bytes),
            None => false,
        },
    };
    if reopened {
        run.verify_reopened();
    }
}
