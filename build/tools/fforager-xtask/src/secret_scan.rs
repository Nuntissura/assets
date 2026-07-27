use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

const MAX_BLOB_BYTES: u64 = 32 * 1024 * 1024;
const MAX_SCAN_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_SCAN_OBJECTS: usize = 250_000;
const MAX_SCAN_COMMITS: usize = 100_000;
const MAX_PRE_PUSH_UPDATES: usize = 10_000;
const MAX_PRE_PUSH_INPUT_BYTES: u64 = 1024 * 1024;
const MAX_GIT_OUTPUT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_REPORTED_FINDINGS: usize = 100;
const GIT_COMMAND_TIMEOUT: Duration = Duration::from_mins(1);
const GIT_BATCH_TIMEOUT: Duration = Duration::from_mins(5);
const MAX_TAG_DEPTH: usize = 16;
const HOOKS_PATH: &str = "build/git-hooks";
const PRE_COMMIT_HOOK: &[u8] = b"#!/bin/sh\nset -eu\nrepo_root=$(git rev-parse --show-toplevel)\ncd \"$repo_root\"\nhook_target=.fforager-artifacts/cargo-target\nif ! CARGO_TARGET_DIR=\"$hook_target\" cargo build --quiet --manifest-path build/Cargo.toml --locked -p fforager-xtask >/dev/null 2>&1; then\n  echo \"FF-SECRET-E-BOOTSTRAP: scanner build failed; compiler output suppressed\" >&2\n  exit 1\nfi\nscanner=\"$hook_target/debug/fforager-xtask\"\nif [ -x \"$scanner.exe\" ]; then scanner=\"$scanner.exe\"; fi\nexec \"$scanner\" secret-scan --staged\n";
const PRE_PUSH_HOOK: &[u8] = b"#!/bin/sh\nset -eu\nremote_name=$1\nrepo_root=$(git rev-parse --show-toplevel)\ncd \"$repo_root\"\nhook_target=.fforager-artifacts/cargo-target\nif ! CARGO_TARGET_DIR=\"$hook_target\" cargo build --quiet --manifest-path build/Cargo.toml --locked -p fforager-xtask >/dev/null 2>&1; then\n  echo \"FF-SECRET-E-BOOTSTRAP: scanner build failed; compiler output suppressed\" >&2\n  exit 1\nfi\nscanner=\"$hook_target/debug/fforager-xtask\"\nif [ -x \"$scanner.exe\" ]; then scanner=\"$scanner.exe\"; fi\nexec \"$scanner\" secret-scan --pre-push \"$remote_name\"\n";

#[derive(Clone, Copy, Debug)]
enum CharacterClass {
    AlphaNumeric,
    Hex,
    UpperAlphaNumeric,
    UrlSafe,
    UrlSafeWithDot,
}

#[derive(Clone, Copy, Debug)]
struct SecretPattern {
    id: &'static str,
    prefix: &'static [u8],
    minimum_tail: usize,
    class: CharacterClass,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Finding {
    pattern_id: &'static str,
    source: String,
    line: usize,
}

#[derive(Clone, Copy, Debug, Default)]
struct ScanBudget {
    objects: usize,
    bytes: u64,
}

impl ScanBudget {
    fn admit(&mut self, source: &str, bytes: u64) -> Result<(), String> {
        self.objects = self
            .objects
            .checked_add(1)
            .ok_or("FF-SECRET-E-SCAN-LIMIT: object count overflow")?;
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .ok_or("FF-SECRET-E-SCAN-LIMIT: byte count overflow")?;
        if self.objects > MAX_SCAN_OBJECTS || self.bytes > MAX_SCAN_BYTES {
            return Err(format!(
                "FF-SECRET-E-SCAN-LIMIT: source={} objects={} bytes={} limits={MAX_SCAN_OBJECTS}/{MAX_SCAN_BYTES}",
                safe_source(source),
                self.objects,
                self.bytes
            ));
        }
        Ok(())
    }
}

const PATTERNS: &[SecretPattern] = &[
    SecretPattern {
        id: "google-api-key",
        prefix: b"AIza",
        minimum_tail: 20,
        class: CharacterClass::UrlSafe,
    },
    SecretPattern {
        id: "google-oauth-client-secret",
        prefix: b"GOCSPX-",
        minimum_tail: 20,
        class: CharacterClass::UrlSafe,
    },
    SecretPattern {
        id: "aws-access-key-id-akia",
        prefix: b"AKIA",
        minimum_tail: 16,
        class: CharacterClass::UpperAlphaNumeric,
    },
    SecretPattern {
        id: "aws-access-key-id-asia",
        prefix: b"ASIA",
        minimum_tail: 16,
        class: CharacterClass::UpperAlphaNumeric,
    },
    SecretPattern {
        id: "github-token-ghp",
        prefix: b"ghp_",
        minimum_tail: 30,
        class: CharacterClass::AlphaNumeric,
    },
    SecretPattern {
        id: "github-token-gho",
        prefix: b"gho_",
        minimum_tail: 30,
        class: CharacterClass::AlphaNumeric,
    },
    SecretPattern {
        id: "github-token-ghu",
        prefix: b"ghu_",
        minimum_tail: 30,
        class: CharacterClass::AlphaNumeric,
    },
    SecretPattern {
        id: "github-token-ghs",
        prefix: b"ghs_",
        minimum_tail: 30,
        class: CharacterClass::AlphaNumeric,
    },
    SecretPattern {
        id: "github-token-ghr",
        prefix: b"ghr_",
        minimum_tail: 30,
        class: CharacterClass::AlphaNumeric,
    },
    SecretPattern {
        id: "github-fine-grained-pat",
        prefix: b"github_pat_",
        minimum_tail: 40,
        class: CharacterClass::UrlSafe,
    },
    SecretPattern {
        id: "gitlab-personal-access-token",
        prefix: b"glpat-",
        minimum_tail: 20,
        class: CharacterClass::UrlSafe,
    },
    SecretPattern {
        id: "slack-token-xoxb",
        prefix: b"xoxb-",
        minimum_tail: 20,
        class: CharacterClass::UrlSafe,
    },
    SecretPattern {
        id: "slack-token-xoxp",
        prefix: b"xoxp-",
        minimum_tail: 20,
        class: CharacterClass::UrlSafe,
    },
    SecretPattern {
        id: "slack-token-xoxa",
        prefix: b"xoxa-",
        minimum_tail: 20,
        class: CharacterClass::UrlSafe,
    },
    SecretPattern {
        id: "slack-token-xoxr",
        prefix: b"xoxr-",
        minimum_tail: 20,
        class: CharacterClass::UrlSafe,
    },
    SecretPattern {
        id: "stripe-secret-key",
        prefix: b"sk_live_",
        minimum_tail: 16,
        class: CharacterClass::AlphaNumeric,
    },
    SecretPattern {
        id: "stripe-restricted-key",
        prefix: b"rk_live_",
        minimum_tail: 16,
        class: CharacterClass::AlphaNumeric,
    },
    SecretPattern {
        id: "openai-project-key",
        prefix: b"sk-proj-",
        minimum_tail: 20,
        class: CharacterClass::UrlSafe,
    },
    SecretPattern {
        id: "openai-service-account-key",
        prefix: b"sk-svcacct-",
        minimum_tail: 20,
        class: CharacterClass::UrlSafe,
    },
    SecretPattern {
        id: "anthropic-api-key",
        prefix: b"sk-ant-api",
        minimum_tail: 20,
        class: CharacterClass::UrlSafe,
    },
    SecretPattern {
        id: "hugging-face-token",
        prefix: b"hf_",
        minimum_tail: 30,
        class: CharacterClass::AlphaNumeric,
    },
    SecretPattern {
        id: "sendgrid-api-key",
        prefix: b"SG.",
        minimum_tail: 50,
        class: CharacterClass::UrlSafeWithDot,
    },
    SecretPattern {
        id: "twilio-api-key",
        prefix: b"SK",
        minimum_tail: 32,
        class: CharacterClass::Hex,
    },
];

pub(crate) fn run(root: &Path, args: &[String]) -> Result<(), String> {
    match args {
        [mode] if mode == "--install-hooks" => install_hooks(root),
        [mode] if mode == "--verify-hooks" => verify_hooks(root),
        [mode] if mode == "--staged" => report(scan_staged(root)?),
        [mode] if mode == "--history" => report(scan_history(root)?),
        [mode, remote] if mode == "--pre-push" => {
            let mut input = String::new();
            io::stdin()
                .take(MAX_PRE_PUSH_INPUT_BYTES + 1)
                .read_to_string(&mut input)
                .map_err(|error| format!("FF-SECRET-E-STDIN: read pre-push updates: {error}"))?;
            if input.len() as u64 > MAX_PRE_PUSH_INPUT_BYTES {
                return Err(format!(
                    "FF-SECRET-E-SCAN-LIMIT: pre-push input exceeds {MAX_PRE_PUSH_INPUT_BYTES} bytes"
                ));
            }
            report(scan_pre_push(root, remote, &input)?)
        }
        _ => Err(
            "usage: fforager-xtask secret-scan <--install-hooks|--verify-hooks|--staged|--history|--pre-push REMOTE>"
                .to_owned(),
        ),
    }
}

pub(crate) fn verify_repository_state(root: &Path) -> Result<usize, String> {
    let mut findings = scan_history(root)?;
    findings.extend(scan_staged(root)?);
    findings.extend(scan_worktree(root)?);
    findings.sort();
    findings.dedup();
    if findings.is_empty() {
        Ok(0)
    } else {
        Err(format_findings(&findings))
    }
}

pub(crate) fn verify_hook_configuration(root: &Path) -> Result<(), String> {
    require_hook_files(root)?;
    require_hook_index_modes(root)?;
    require_hook_index_contents(root)?;
    let configured = git_text(root, &["config", "--get", "core.hooksPath"])?;
    if configured.trim() != HOOKS_PATH {
        return Err(format!(
            "FF-SECRET-E-HOOKS-PATH: expected core.hooksPath={HOOKS_PATH}; run `cargo run --manifest-path build/Cargo.toml --locked -p fforager-xtask -- secret-scan --install-hooks`"
        ));
    }
    Ok(())
}

fn install_hooks(root: &Path) -> Result<(), String> {
    require_hook_files(root)?;
    git_status(root, &["config", "--local", "core.hooksPath", HOOKS_PATH])?;
    verify_hooks(root)?;
    println!("PASS FF-GATE-SECRET-001; hooks_path={HOOKS_PATH}");
    Ok(())
}

fn verify_hooks(root: &Path) -> Result<(), String> {
    verify_hook_configuration(root)?;
    println!("PASS FF-GATE-SECRET-001; hooks_path={HOOKS_PATH}");
    Ok(())
}

fn require_hook_index_modes(root: &Path) -> Result<(), String> {
    for hook in ["pre-commit", "pre-push"] {
        let relative = format!("{HOOKS_PATH}/{hook}");
        let row = git_text(root, &["ls-files", "--stage", "--", &relative])?;
        if !row.starts_with("100755 ") {
            let observed_mode = row.split_whitespace().next().unwrap_or("<missing>");
            return Err(format!(
                "FF-SECRET-E-HOOK-MODE: expected executable index mode 100755 for {relative}; observed {observed_mode}"
            ));
        }
    }
    Ok(())
}

fn require_hook_index_contents(root: &Path) -> Result<(), String> {
    for (hook, expected) in [("pre-commit", PRE_COMMIT_HOOK), ("pre-push", PRE_PUSH_HOOK)] {
        let relative = format!("{HOOKS_PATH}/{hook}");
        let actual = git_bytes(root, &["show", &format!(":{relative}")])?;
        if actual != expected {
            return Err(format!(
                "FF-SECRET-E-HOOK-INDEX-CONTENT: staged hook content differs: {relative}"
            ));
        }
    }
    Ok(())
}

fn require_hook_files(root: &Path) -> Result<(), String> {
    for (hook, expected) in [("pre-commit", PRE_COMMIT_HOOK), ("pre-push", PRE_PUSH_HOOK)] {
        let path = root.join(HOOKS_PATH).join(hook);
        let display = slash(&path);
        if !path.is_file() {
            return Err(format!(
                "FF-SECRET-E-HOOK-MISSING: required hook is absent: {}",
                safe_source(&display)
            ));
        }
        let actual = fs::read(&path).map_err(|error| {
            format!("FF-SECRET-E-HOOK-READ: {}: {error}", safe_source(&display))
        })?;
        if actual != expected {
            return Err(format!(
                "FF-SECRET-E-HOOK-CONTENT: required hook content differs: {}",
                safe_source(&display)
            ));
        }
    }
    Ok(())
}

fn scan_staged(root: &Path) -> Result<Vec<Finding>, String> {
    require_hook_files(root)?;
    require_hook_index_modes(root)?;
    require_hook_index_contents(root)?;
    let rows = git_bytes(root, &["ls-files", "--stage", "-z"])?;
    let mut blobs = BTreeMap::<String, String>::new();
    let mut findings = Vec::new();
    let mut budget = ScanBudget::default();
    for row in rows.split(|byte| *byte == 0).filter(|row| !row.is_empty()) {
        let tab = row
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or("FF-SECRET-E-INDEX: malformed ls-files row")?;
        let header = std::str::from_utf8(&row[..tab])
            .map_err(|_| "FF-SECRET-E-INDEX: non-UTF-8 ls-files header")?;
        let path = std::str::from_utf8(&row[tab + 1..])
            .map_err(|_| "FF-SECRET-E-PATH: non-UTF-8 repository path")?;
        let fields = header.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 3 || fields[2] != "0" {
            return Err(format!(
                "FF-SECRET-E-INDEX: unresolved or malformed index stage for {}",
                safe_source(path)
            ));
        }
        validate_object_id(fields[1])?;
        findings.extend(scan_bytes("staged-path:<path>", path.as_bytes()));
        blobs
            .entry(fields[1].to_owned())
            .or_insert_with(|| format!("staged:{path}"));
    }
    scan_objects_batch(root, blobs, Some("blob"), &mut findings, &mut budget)?;
    Ok(findings)
}

fn scan_history(root: &Path) -> Result<Vec<Finding>, String> {
    let commits = lines(&git_text(root, &["rev-list", "--all"])?);
    let mut findings = scan_commits(root, &commits)?;
    let ref_rows = lines(&git_text(
        root,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    )?);
    let mut budget = ScanBudget::default();
    for row in ref_rows {
        let (ref_name, object_id) = row
            .split_once(' ')
            .ok_or("FF-SECRET-E-REF: malformed for-each-ref output")?;
        findings.extend(scan_bytes("history-ref:<ref>", ref_name.as_bytes()));
        validate_object_id(object_id)?;
        if git_text(root, &["cat-file", "-t", object_id])?.trim() != "commit" {
            scan_git_object_chain(
                root,
                object_id,
                &format!("history-object:{object_id}"),
                &mut findings,
                &mut budget,
            )?;
        }
    }
    Ok(findings)
}

fn scan_pre_push(root: &Path, remote: &str, input: &str) -> Result<Vec<Finding>, String> {
    verify_hook_configuration(root)?;
    if remote.trim().is_empty() {
        return Err("FF-SECRET-E-PRE-PUSH: remote name is empty".to_owned());
    }
    let mut findings = Vec::new();
    let mut commits = BTreeSet::new();
    let mut pushed_objects = BTreeSet::new();
    let updates = input.lines().filter(|line| !line.trim().is_empty());
    for (index, line) in updates.enumerate() {
        if index >= MAX_PRE_PUSH_UPDATES {
            return Err(format!(
                "FF-SECRET-E-SCAN-LIMIT: pre-push updates exceed {MAX_PRE_PUSH_UPDATES}"
            ));
        }
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 4 {
            return Err("FF-SECRET-E-PRE-PUSH: malformed hook update row".to_owned());
        }
        let local_object = fields[1];
        let remote_object = fields[3];
        findings.extend(scan_bytes("pre-push-local-ref:<ref>", fields[0].as_bytes()));
        findings.extend(scan_bytes(
            "pre-push-remote-ref:<ref>",
            fields[2].as_bytes(),
        ));
        if is_zero_object_id(local_object) {
            continue;
        }
        validate_object_id(local_object)?;
        validate_object_id(remote_object)?;
        pushed_objects.insert(local_object.to_owned());
        let output = if is_zero_object_id(remote_object) {
            git_text(root, &["rev-list", local_object])?
        } else {
            git_text(
                root,
                &["rev-list", local_object, &format!("^{remote_object}")],
            )?
        };
        commits.extend(lines(&output));
        if commits.len() > MAX_SCAN_COMMITS {
            return Err(format!(
                "FF-SECRET-E-SCAN-LIMIT: outgoing commits exceed {MAX_SCAN_COMMITS}"
            ));
        }
    }
    findings.extend(scan_commits(
        root,
        &commits.into_iter().collect::<Vec<_>>(),
    )?);
    let mut budget = ScanBudget::default();
    for object_id in pushed_objects {
        scan_git_object_chain(
            root,
            &object_id,
            &format!("pushed-object:{object_id}"),
            &mut findings,
            &mut budget,
        )?;
    }
    Ok(findings)
}

fn scan_worktree(root: &Path) -> Result<Vec<Finding>, String> {
    let paths = nul_strings(&git_bytes(
        root,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    )?)?;
    let mut findings = Vec::new();
    let mut budget = ScanBudget::default();
    for relative in paths {
        findings.extend(scan_bytes("worktree-path:<path>", relative.as_bytes()));
        let path = root.join(&relative);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // The exact index is scanned separately. A tracked path that is
                // deleted from the worktree has no additional bytes to inspect.
                continue;
            }
            Err(error) => {
                return Err(format!(
                    "FF-SECRET-E-WORKTREE: inspect {}: {error}",
                    safe_source(&relative)
                ));
            }
        };
        if metadata.file_type().is_symlink() {
            let target = fs::read_link(&path).map_err(|error| {
                format!(
                    "FF-SECRET-E-WORKTREE: read symlink {}: {error}",
                    safe_source(&relative)
                )
            })?;
            budget.admit(
                &format!("worktree:{relative}"),
                target.as_os_str().len() as u64,
            )?;
            findings.extend(scan_bytes(
                &format!("worktree:{relative}"),
                target.to_string_lossy().as_bytes(),
            ));
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        if metadata.len() > MAX_BLOB_BYTES {
            return Err(format!(
                "FF-SECRET-E-BLOB-LIMIT: worktree:{} is {} bytes; maximum scannable size is {MAX_BLOB_BYTES}",
                safe_source(&relative),
                metadata.len()
            ));
        }
        budget.admit(&format!("worktree:{relative}"), metadata.len())?;
        let file = fs::File::open(&path).map_err(|error| {
            format!(
                "FF-SECRET-E-WORKTREE: read {}: {error}",
                safe_source(&relative)
            )
        })?;
        let mut bytes = Vec::new();
        file.take(MAX_BLOB_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                format!(
                    "FF-SECRET-E-WORKTREE: read {}: {error}",
                    safe_source(&relative)
                )
            })?;
        if bytes.len() as u64 > MAX_BLOB_BYTES {
            return Err(format!(
                "FF-SECRET-E-BLOB-LIMIT: worktree:{} exceeded {MAX_BLOB_BYTES} bytes while reading",
                safe_source(&relative)
            ));
        }
        findings.extend(scan_bytes(&format!("worktree:{relative}"), &bytes));
    }
    Ok(findings)
}

fn scan_commits(root: &Path, commits: &[String]) -> Result<Vec<Finding>, String> {
    if commits.len() > MAX_SCAN_COMMITS {
        return Err(format!(
            "FF-SECRET-E-SCAN-LIMIT: commits={} limit={MAX_SCAN_COMMITS}",
            commits.len()
        ));
    }
    let mut blobs = BTreeMap::<String, String>::new();
    let mut findings = Vec::new();
    let mut budget = ScanBudget::default();
    let mut commit_objects = BTreeMap::new();
    for commit in commits {
        validate_object_id(commit)?;
        commit_objects.insert(commit.to_owned(), format!("commit-object:{commit}"));
    }
    scan_objects_batch(
        root,
        commit_objects,
        Some("commit"),
        &mut findings,
        &mut budget,
    )?;
    for commit in commits {
        let tree = git_bytes(root, &["ls-tree", "-r", "-z", "--full-tree", commit])?;
        for row in tree.split(|byte| *byte == 0).filter(|row| !row.is_empty()) {
            let tab = row
                .iter()
                .position(|byte| *byte == b'\t')
                .ok_or("FF-SECRET-E-TREE: malformed ls-tree row")?;
            let header = std::str::from_utf8(&row[..tab])
                .map_err(|_| "FF-SECRET-E-TREE: non-UTF-8 ls-tree header")?;
            let path = std::str::from_utf8(&row[tab + 1..])
                .map_err(|_| "FF-SECRET-E-TREE: non-UTF-8 repository path")?;
            findings.extend(scan_bytes("commit-path:<path>", path.as_bytes()));
            let fields = header.split_whitespace().collect::<Vec<_>>();
            if fields.len() != 3 || fields[1] != "blob" {
                continue;
            }
            validate_object_id(fields[2])?;
            blobs
                .entry(fields[2].to_owned())
                .or_insert_with(|| format!("commit:{commit}:{path}"));
            if blobs.len() > MAX_SCAN_OBJECTS {
                return Err(format!(
                    "FF-SECRET-E-SCAN-LIMIT: unique blobs exceed {MAX_SCAN_OBJECTS}"
                ));
            }
        }
    }
    scan_objects_batch(root, blobs, Some("blob"), &mut findings, &mut budget)?;
    Ok(findings)
}

fn scan_objects_batch(
    root: &Path,
    objects: BTreeMap<String, String>,
    expected_type: Option<&'static str>,
    findings: &mut Vec<Finding>,
    budget: &mut ScanBudget,
) -> Result<(), String> {
    if objects.is_empty() {
        return Ok(());
    }
    if objects.len() > MAX_SCAN_OBJECTS {
        return Err(format!(
            "FF-SECRET-E-SCAN-LIMIT: batch objects exceed {MAX_SCAN_OBJECTS}"
        ));
    }
    let mut input = Vec::with_capacity(objects.len() * 65);
    for object_id in objects.keys() {
        validate_object_id(object_id)?;
        input.extend_from_slice(object_id.as_bytes());
        input.push(b'\n');
    }
    let mut child = Command::new("git")
        .args(["cat-file", "--batch"])
        .current_dir(root)
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("FF-SECRET-E-GIT: start git cat-file batch: {error}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or("FF-SECRET-E-GIT: Git batch stdin pipe is unavailable")?;
    let stdout = child
        .stdout
        .take()
        .ok_or("FF-SECRET-E-GIT: Git batch stdout pipe is unavailable")?;
    let writer = thread::spawn(move || {
        let result = stdin.write_all(&input);
        drop(stdin);
        result
    });
    let initial_budget = *budget;
    let reader =
        thread::spawn(move || read_batch_objects(stdout, objects, expected_type, initial_budget));
    let status = super::wait_for_child(&mut child, "git", &["cat-file"], GIT_BATCH_TIMEOUT);
    let read_result = reader
        .join()
        .map_err(|_| "FF-SECRET-E-GIT: Git batch reader panicked".to_owned())?;
    let write_result = writer
        .join()
        .map_err(|_| "FF-SECRET-E-GIT: Git batch writer panicked".to_owned())?;
    let (batch_findings, final_budget) = read_result?;
    write_result.map_err(|error| format!("FF-SECRET-E-GIT: write batch input: {error}"))?;
    let status = status.map_err(|_| {
        format!(
            "FF-SECRET-E-GIT-TIMEOUT: git cat-file exceeded {} seconds",
            GIT_BATCH_TIMEOUT.as_secs()
        )
    })?;
    if !status.success() {
        return Err(format!(
            "FF-SECRET-E-GIT: git cat-file exited {status}; stderr suppressed"
        ));
    }
    findings.extend(batch_findings);
    *budget = final_budget;
    Ok(())
}

fn read_batch_objects(
    stdout: impl Read,
    mut sources: BTreeMap<String, String>,
    expected_type: Option<&str>,
    mut budget: ScanBudget,
) -> Result<(Vec<Finding>, ScanBudget), String> {
    let mut output = BufReader::new(stdout);
    let mut findings = Vec::new();
    while !sources.is_empty() {
        let mut header = Vec::new();
        let header_bytes = output
            .read_until(b'\n', &mut header)
            .map_err(|error| format!("FF-SECRET-E-GIT: read batch header: {error}"))?;
        if header_bytes == 0 || header_bytes > 256 {
            return Err("FF-SECRET-E-GIT: malformed Git batch header".to_owned());
        }
        let header = std::str::from_utf8(&header)
            .map_err(|_| "FF-SECRET-E-GIT: non-UTF-8 Git batch header")?;
        let fields = header.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 3 {
            return Err("FF-SECRET-E-GIT: missing or malformed Git batch object".to_owned());
        }
        validate_object_id(fields[0])?;
        if expected_type.is_some_and(|expected| fields[1] != expected) {
            return Err(format!(
                "FF-SECRET-E-OBJECT-TYPE: expected {}, observed {}",
                expected_type.unwrap_or("<object>"),
                fields[1]
            ));
        }
        let source = sources
            .remove(fields[0])
            .ok_or("FF-SECRET-E-GIT: unexpected or duplicate Git batch object")?;
        let size = fields[2]
            .parse::<u64>()
            .map_err(|error| format!("FF-SECRET-E-OBJECT-SIZE: parse batch size: {error}"))?;
        if size > MAX_BLOB_BYTES {
            return Err(format!(
                "FF-SECRET-E-OBJECT-LIMIT: {} is {size} bytes; maximum scannable size is {MAX_BLOB_BYTES}",
                safe_source(&source)
            ));
        }
        budget.admit(&source, size)?;
        let allocation = usize::try_from(size)
            .map_err(|_| "FF-SECRET-E-OBJECT-LIMIT: object size exceeds address space")?;
        let mut bytes = vec![0; allocation];
        output
            .read_exact(&mut bytes)
            .map_err(|error| format!("FF-SECRET-E-GIT: read batch object: {error}"))?;
        let mut terminator = [0_u8; 1];
        output
            .read_exact(&mut terminator)
            .map_err(|error| format!("FF-SECRET-E-GIT: read batch terminator: {error}"))?;
        if terminator != *b"\n" {
            return Err("FF-SECRET-E-GIT: malformed Git batch terminator".to_owned());
        }
        findings.extend(scan_bytes(&source, &bytes));
    }
    Ok((findings, budget))
}

fn scan_git_object(
    root: &Path,
    object_id: &str,
    source: &str,
    findings: &mut Vec<Finding>,
    budget: &mut ScanBudget,
) -> Result<Vec<u8>, String> {
    let size = git_text(root, &["cat-file", "-s", object_id])?
        .trim()
        .parse::<u64>()
        .map_err(|error| format!("FF-SECRET-E-OBJECT-SIZE: parse object size: {error}"))?;
    if size > MAX_BLOB_BYTES {
        return Err(format!(
            "FF-SECRET-E-OBJECT-LIMIT: {} is {size} bytes; maximum scannable size is {MAX_BLOB_BYTES}",
            safe_source(source)
        ));
    }
    budget.admit(source, size)?;
    let bytes = git_bytes(root, &["cat-file", "-p", object_id])?;
    findings.extend(scan_bytes(source, &bytes));
    Ok(bytes)
}

fn scan_git_object_chain(
    root: &Path,
    initial_object_id: &str,
    source: &str,
    findings: &mut Vec<Finding>,
    budget: &mut ScanBudget,
) -> Result<(), String> {
    let mut current = initial_object_id.to_owned();
    let mut visited = BTreeSet::new();
    for depth in 0..MAX_TAG_DEPTH {
        if !visited.insert(current.clone()) {
            return Err("FF-SECRET-E-TAG-CYCLE: tag object chain contains a cycle".to_owned());
        }
        let object_type = git_text(root, &["cat-file", "-t", &current])?;
        let bytes = scan_git_object(
            root,
            &current,
            &format!("{source}:depth={depth}"),
            findings,
            budget,
        )?;
        match object_type.trim() {
            "tag" => {}
            "tree" => {
                scan_tree(root, &current, &format!("{source}:tree"), findings, budget)?;
                return Ok(());
            }
            "commit" | "blob" => return Ok(()),
            _ => {
                return Err(
                    "FF-SECRET-E-OBJECT-TYPE: unsupported pushed Git object type".to_owned(),
                );
            }
        }
        let header = bytes
            .split(|byte| *byte == b'\n')
            .next()
            .ok_or("FF-SECRET-E-TAG: tag object has no object header")?;
        let text = std::str::from_utf8(header)
            .map_err(|_| "FF-SECRET-E-TAG: non-UTF-8 tag object header")?;
        let next = text
            .strip_prefix("object ")
            .ok_or("FF-SECRET-E-TAG: malformed tag object header")?;
        validate_object_id(next)?;
        next.clone_into(&mut current);
    }
    Err(format!(
        "FF-SECRET-E-SCAN-LIMIT: tag depth exceeds {MAX_TAG_DEPTH}"
    ))
}

fn scan_tree(
    root: &Path,
    tree_id: &str,
    source: &str,
    findings: &mut Vec<Finding>,
    budget: &mut ScanBudget,
) -> Result<(), String> {
    let tree = git_bytes(root, &["ls-tree", "-r", "-z", "--full-tree", tree_id])?;
    let mut blobs = BTreeMap::<String, String>::new();
    for row in tree.split(|byte| *byte == 0).filter(|row| !row.is_empty()) {
        let tab = row
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or("FF-SECRET-E-TREE: malformed ls-tree row")?;
        let header = std::str::from_utf8(&row[..tab])
            .map_err(|_| "FF-SECRET-E-TREE: non-UTF-8 ls-tree header")?;
        let path = std::str::from_utf8(&row[tab + 1..])
            .map_err(|_| "FF-SECRET-E-PATH: non-UTF-8 repository path")?;
        findings.extend(scan_bytes("tree-path:<path>", path.as_bytes()));
        let fields = header.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 3 || fields[1] != "blob" {
            continue;
        }
        validate_object_id(fields[2])?;
        blobs
            .entry(fields[2].to_owned())
            .or_insert_with(|| format!("{source}:<path>"));
        if blobs.len() > MAX_SCAN_OBJECTS {
            return Err(format!(
                "FF-SECRET-E-SCAN-LIMIT: tree blobs exceed {MAX_SCAN_OBJECTS}"
            ));
        }
    }
    scan_objects_batch(root, blobs, Some("blob"), findings, budget)?;
    Ok(())
}

fn scan_bytes(source: &str, bytes: &[u8]) -> Vec<Finding> {
    let mut findings = BTreeSet::new();
    for pattern in PATTERNS {
        if bytes.len() < pattern.prefix.len() + pattern.minimum_tail {
            continue;
        }
        let mut line = 1;
        for start in 0..=bytes.len() - pattern.prefix.len() {
            if start > 0 && bytes[start - 1] == b'\n' {
                line += 1;
            }
            if bytes[start..].starts_with(pattern.prefix)
                && tail_length(bytes, start + pattern.prefix.len(), pattern.class)
                    >= pattern.minimum_tail
            {
                findings.insert(Finding {
                    pattern_id: pattern.id,
                    source: source.to_owned(),
                    line,
                });
                if findings.len() >= MAX_REPORTED_FINDINGS {
                    return findings.into_iter().collect();
                }
            }
        }
        for little_endian in [true, false] {
            let encoded_prefix_bytes = pattern.prefix.len() * 2;
            if bytes.len() < encoded_prefix_bytes + pattern.minimum_tail * 2 {
                continue;
            }
            let mut line = 1;
            for start in 0..=bytes.len() - encoded_prefix_bytes {
                if start > 0 && bytes[start - 1] == b'\n' {
                    line += 1;
                }
                if encoded_prefix_matches(bytes, start, pattern.prefix, little_endian)
                    && encoded_tail_length(
                        bytes,
                        start + encoded_prefix_bytes,
                        pattern.class,
                        little_endian,
                    ) >= pattern.minimum_tail
                {
                    findings.insert(Finding {
                        pattern_id: pattern.id,
                        source: source.to_owned(),
                        line,
                    });
                    if findings.len() >= MAX_REPORTED_FINDINGS {
                        return findings.into_iter().collect();
                    }
                }
            }
        }
    }
    findings.into_iter().collect()
}

fn encoded_prefix_matches(bytes: &[u8], start: usize, prefix: &[u8], little_endian: bool) -> bool {
    prefix.iter().enumerate().all(|(offset, expected)| {
        encoded_ascii_byte(bytes, start + offset * 2, little_endian) == Some(*expected)
    })
}

fn encoded_tail_length(
    bytes: &[u8],
    start: usize,
    class: CharacterClass,
    little_endian: bool,
) -> usize {
    (start..bytes.len())
        .step_by(2)
        .map_while(|offset| encoded_ascii_byte(bytes, offset, little_endian))
        .take_while(|byte| allowed(*byte, class))
        .count()
}

fn encoded_ascii_byte(bytes: &[u8], offset: usize, little_endian: bool) -> Option<u8> {
    let pair = bytes.get(offset..offset + 2)?;
    let (value, zero) = if little_endian {
        (pair[0], pair[1])
    } else {
        (pair[1], pair[0])
    };
    (zero == 0 && value.is_ascii()).then_some(value)
}

fn tail_length(bytes: &[u8], start: usize, class: CharacterClass) -> usize {
    bytes[start..]
        .iter()
        .take_while(|byte| allowed(**byte, class))
        .count()
}

fn allowed(byte: u8, class: CharacterClass) -> bool {
    match class {
        CharacterClass::AlphaNumeric => byte.is_ascii_alphanumeric(),
        CharacterClass::Hex => byte.is_ascii_hexdigit(),
        CharacterClass::UpperAlphaNumeric => byte.is_ascii_uppercase() || byte.is_ascii_digit(),
        CharacterClass::UrlSafe => byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'),
        CharacterClass::UrlSafeWithDot => {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
        }
    }
}

fn validate_object_id(value: &str) -> Result<(), String> {
    if !matches!(value.len(), 40 | 64) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("FF-SECRET-E-OBJECT-ID: malformed Git object ID".to_owned());
    }
    Ok(())
}

fn is_zero_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte == b'0')
}

fn report(mut findings: Vec<Finding>) -> Result<(), String> {
    findings.sort();
    findings.dedup();
    if findings.is_empty() {
        println!("PASS FF-GATE-SECRET-001; findings=0");
        Ok(())
    } else {
        Err(format_findings(&findings))
    }
}

fn format_findings(findings: &[Finding]) -> String {
    let diagnostics = findings
        .iter()
        .take(100)
        .map(|finding| {
            format!(
                "{} at {}:{}",
                finding.pattern_id,
                safe_source(&finding.source),
                finding.line
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "FF-SECRET-E-DETECTED: {} potential API key(s); matched values suppressed; {diagnostics}",
        findings.len()
    )
}

pub(crate) fn credential_snapshot_counterfactual_diagnostic(root: &Path) -> Result<(), String> {
    git_status(root, &["init", "--quiet"])?;
    git_status(root, &["config", "core.autocrlf", "false"])?;
    fs::create_dir_all(root.join(HOOKS_PATH))
        .map_err(|error| format!("FF-SECRET-E-COUNTERFACTUAL: create hooks: {error}"))?;
    fs::write(root.join(HOOKS_PATH).join("pre-commit"), PRE_COMMIT_HOOK)
        .map_err(|error| format!("FF-SECRET-E-COUNTERFACTUAL: write pre-commit: {error}"))?;
    fs::write(root.join(HOOKS_PATH).join("pre-push"), PRE_PUSH_HOOK)
        .map_err(|error| format!("FF-SECRET-E-COUNTERFACTUAL: write pre-push: {error}"))?;
    git_status(root, &["add", "build/git-hooks/pre-commit"])?;
    git_status(root, &["add", "build/git-hooks/pre-push"])?;
    git_status(root, &["config", "--local", "core.hooksPath", HOOKS_PATH])?;

    let key = format!("{}{}", ["AI", "za"].concat(), "Z".repeat(35));
    fs::write(root.join("credential.txt"), &key)
        .map_err(|error| format!("FF-SECRET-E-COUNTERFACTUAL: write staged input: {error}"))?;
    git_status(root, &["add", "credential.txt"])?;
    force_counterfactual_hook_mode(root, "build/git-hooks/pre-commit")?;
    force_counterfactual_hook_mode(root, "build/git-hooks/pre-push")?;
    let staged = scan_staged(root)?;
    if staged.len() != 1 {
        return Err(format!(
            "FF-SECRET-E-COUNTERFACTUAL: staged boundary expected one finding, observed {}",
            staged.len()
        ));
    }

    let secret_tree = git_text(root, &["write-tree"])?;
    let zero = "0000000000000000000000000000000000000000";
    let tree_findings = scan_pre_push(
        root,
        "origin",
        &format!(
            "refs/tags/credential-tree {} refs/tags/credential-tree {zero}\n",
            secret_tree.trim()
        ),
    )?;
    if tree_findings.is_empty() {
        return Err(
            "FF-SECRET-E-COUNTERFACTUAL: outgoing tree boundary accepted a secret blob".to_owned(),
        );
    }

    git_status(root, &["rm", "--cached", "--quiet", "credential.txt"])?;
    fs::write(root.join("clean.txt"), b"clean")
        .map_err(|error| format!("FF-SECRET-E-COUNTERFACTUAL: write clean input: {error}"))?;
    git_status(root, &["add", "clean.txt"])?;
    let clean_tree = git_text(root, &["write-tree"])?;
    let ref_findings = scan_pre_push(
        root,
        "origin",
        &format!(
            "refs/heads/{key} {} refs/heads/credential-ref {zero}\n",
            clean_tree.trim()
        ),
    )?;
    if ref_findings.is_empty() {
        return Err(
            "FF-SECRET-E-COUNTERFACTUAL: outgoing ref boundary accepted a secret name".to_owned(),
        );
    }

    let mut findings = staged;
    findings.extend(tree_findings);
    findings.extend(ref_findings);
    let diagnostic = format_findings(&findings);
    if diagnostic.contains(&key) {
        return Err("FF-SECRET-E-COUNTERFACTUAL: diagnostic leaked matched value".to_owned());
    }
    Err(diagnostic)
}

fn force_counterfactual_hook_mode(root: &Path, relative: &str) -> Result<(), String> {
    git_status(root, &["add", "--chmod=+x", "--", relative])
}

fn git_text(root: &Path, args: &[&str]) -> Result<String, String> {
    String::from_utf8(git_bytes(root, args)?).map_err(|_| {
        format!(
            "FF-SECRET-E-GIT: non-UTF-8 output from git {}",
            git_subcommand(args)
        )
    })
}

fn git_bytes(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            format!(
                "FF-SECRET-E-GIT: start git {}: {error}",
                git_subcommand(args)
            )
        })?;
    let stdout = child
        .stdout
        .take()
        .ok_or("FF-SECRET-E-GIT: Git stdout pipe is unavailable")?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take(MAX_GIT_OUTPUT_BYTES + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let subcommand = git_subcommand(args);
    let safe_args = [subcommand.as_str()];
    let status = super::wait_for_child(&mut child, "git", &safe_args, GIT_COMMAND_TIMEOUT);
    let bytes = reader
        .join()
        .map_err(|_| "FF-SECRET-E-GIT: Git output reader panicked".to_owned())?
        .map_err(|error| format!("FF-SECRET-E-GIT: read Git output: {error}"))?;
    if bytes.len() as u64 > MAX_GIT_OUTPUT_BYTES {
        return Err(format!(
            "FF-SECRET-E-SCAN-LIMIT: git {subcommand} output exceeds {MAX_GIT_OUTPUT_BYTES} bytes"
        ));
    }
    let status = status.map_err(|_| {
        format!(
            "FF-SECRET-E-GIT-TIMEOUT: git {subcommand} exceeded {} seconds",
            GIT_COMMAND_TIMEOUT.as_secs()
        )
    })?;
    if !status.success() {
        return Err(format!(
            "FF-SECRET-E-GIT: git {subcommand} exited {status}; stderr suppressed",
        ));
    }
    Ok(bytes)
}

fn git_status(root: &Path, args: &[&str]) -> Result<(), String> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            format!(
                "FF-SECRET-E-GIT: start git {}: {error}",
                git_subcommand(args)
            )
        })?;
    let subcommand = git_subcommand(args);
    let safe_args = [subcommand.as_str()];
    let status = super::wait_for_child(&mut child, "git", &safe_args, GIT_COMMAND_TIMEOUT)
        .map_err(|_| {
            format!(
                "FF-SECRET-E-GIT-TIMEOUT: git {subcommand} exceeded {} seconds",
                GIT_COMMAND_TIMEOUT.as_secs()
            )
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "FF-SECRET-E-GIT: git {} exited {status}",
            git_subcommand(args)
        ))
    }
}

fn nul_strings(bytes: &[u8]) -> Result<Vec<String>, String> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|row| !row.is_empty())
        .map(|row| {
            std::str::from_utf8(row)
                .map(str::to_owned)
                .map_err(|_| "FF-SECRET-E-PATH: non-UTF-8 repository path".to_owned())
        })
        .collect()
}

fn lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn git_subcommand(args: &[&str]) -> String {
    args.first().copied().unwrap_or("<missing>").to_owned()
}

fn safe_source(source: &str) -> &str {
    if contains_secret_pattern(source.as_bytes()) {
        "<redacted-source>"
    } else {
        source
    }
}

fn contains_secret_pattern(bytes: &[u8]) -> bool {
    PATTERNS.iter().any(|pattern| {
        bytes
            .windows(pattern.prefix.len())
            .enumerate()
            .any(|(start, window)| {
                window == pattern.prefix
                    && tail_length(bytes, start + pattern.prefix.len(), pattern.class)
                        >= pattern.minimum_tail
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn provider_patterns_detect_without_echoing_secret() {
        let secret = format!("{}{}", ["AI", "za"].concat(), "A".repeat(35));
        let findings = scan_bytes("fixture.txt", secret.as_bytes());
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].pattern_id, "google-api-key");
        assert_eq!(
            scan_bytes("embedded.txt", format!("x{secret}").as_bytes()).len(),
            1
        );
        let diagnostic = format_findings(&findings);
        assert!(diagnostic.contains("matched values suppressed"));
        assert!(!diagnostic.contains(&secret));
    }

    #[test]
    fn secret_in_source_label_is_suppressed() {
        let secret = format!("{}{}", ["AI", "za"].concat(), "E".repeat(35));
        let findings = vec![Finding {
            pattern_id: "google-api-key",
            source: format!("staged:{secret}.txt"),
            line: 1,
        }];
        let diagnostic = format_findings(&findings);
        assert!(diagnostic.contains("<redacted-source>"));
        assert!(!diagnostic.contains(&secret));
    }

    #[test]
    fn redacted_placeholder_and_short_prefix_pass() {
        assert!(scan_bytes("fixture.txt", b"REDACTED_GOOGLE_API_KEY and AIza-short").is_empty());
    }

    #[test]
    fn binary_content_is_scanned() {
        let secret = format!("prefix\0{}{}", ["gh", "p_"].concat(), "B".repeat(36));
        let findings = scan_bytes("fixture.bin", secret.as_bytes());
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].pattern_id, "github-token-ghp");
    }

    #[test]
    fn dense_provider_matches_are_bounded() {
        let secret = format!("{}{}", ["AI", "za"].concat(), "Q".repeat(35));
        let input = format!("{secret}\n").repeat(MAX_REPORTED_FINDINGS * 10);
        let findings = scan_bytes("dense-fixture.txt", input.as_bytes());
        assert_eq!(findings.len(), MAX_REPORTED_FINDINGS);
        assert!(!format_findings(&findings).contains(&secret));
    }

    #[test]
    fn credential_snapshot_counterfactual_executes_git_boundaries() {
        let root = temp_repo("credential-boundary");
        let diagnostic = credential_snapshot_counterfactual_diagnostic(&root)
            .expect_err("isolated Git credential boundaries must reject");
        assert!(
            diagnostic.starts_with("FF-SECRET-E-DETECTED:"),
            "{diagnostic}"
        );
        assert!(diagnostic.contains("matched values suppressed"));
        fs::remove_dir_all(&root).expect("remove fixture repository");
    }

    #[test]
    fn utf16_and_gitlab_provider_tokens_are_scanned() {
        let google = format!("{}{}", ["AI", "za"].concat(), "M".repeat(35));
        let mut little_endian_bytes = vec![0xff, 0xfe];
        for byte in google.bytes() {
            little_endian_bytes.extend([byte, 0]);
        }
        let mut big_endian_bytes = vec![0xfe, 0xff];
        for byte in google.bytes() {
            big_endian_bytes.extend([0, byte]);
        }
        assert_eq!(scan_bytes("utf16-le.txt", &little_endian_bytes).len(), 1);
        assert_eq!(scan_bytes("utf16-be.txt", &big_endian_bytes).len(), 1);

        let gitlab = format!("{}{}", ["gl", "pat-"].concat(), "N".repeat(20));
        let findings = scan_bytes("gitlab.txt", gitlab.as_bytes());
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].pattern_id, "gitlab-personal-access-token");
    }

    #[test]
    fn staged_and_history_scans_reject_a_key() {
        let root = temp_repo("staged-history");
        git_ok(&root, &["init", "--quiet"]);
        git_ok(&root, &["config", "user.name", "Ferric Test"]);
        git_ok(&root, &["config", "user.email", "ferric@example.invalid"]);
        install_test_hooks(&root);

        let secret = format!("{}{}", ["AI", "za"].concat(), "C".repeat(35));
        fs::write(root.join("credential.txt"), &secret).expect("write fixture");
        git_ok(&root, &["add", "credential.txt"]);
        let staged = scan_staged(&root).expect("scan staged");
        assert_eq!(staged.len(), 1);

        git_ok(
            &root,
            &["commit", "--quiet", "--no-verify", "-m", "fixture"],
        );
        let history = scan_history(&root).expect("scan history");
        assert_eq!(history.len(), 1);
        let diagnostic = format_findings(&history);
        assert!(!diagnostic.contains(&secret));

        fs::remove_dir_all(&root).expect("remove fixture repository");
    }

    #[test]
    fn clean_staged_blob_passes() {
        let root = temp_repo("clean-staged");
        git_ok(&root, &["init", "--quiet"]);
        install_test_hooks(&root);
        fs::write(root.join("clean.txt"), b"REDACTED_GOOGLE_API_KEY").expect("write fixture");
        git_ok(&root, &["add", "clean.txt"]);
        assert!(scan_staged(&root).expect("scan staged").is_empty());
        fs::remove_dir_all(&root).expect("remove fixture repository");
    }

    #[test]
    fn complete_index_and_secret_bearing_path_are_scanned() {
        let root = temp_repo("complete-index");
        git_ok(&root, &["init", "--quiet"]);
        git_ok(&root, &["config", "user.name", "Ferric Test"]);
        git_ok(&root, &["config", "user.email", "ferric@example.invalid"]);
        install_test_hooks(&root);

        let secret = format!("{}{}", ["AI", "za"].concat(), "F".repeat(35));
        fs::write(root.join("credential.txt"), &secret).expect("write indexed secret");
        git_ok(&root, &["add", "credential.txt"]);
        git_ok(
            &root,
            &["commit", "--quiet", "--no-verify", "-m", "indexed secret"],
        );
        fs::write(root.join("credential.txt"), b"clean worktree").expect("clean worktree only");
        fs::write(root.join("unrelated.txt"), b"clean").expect("write unrelated staged file");
        git_ok(&root, &["add", "unrelated.txt"]);
        assert!(!scan_staged(&root).expect("scan complete index").is_empty());

        let secret_path = format!("{secret}.txt");
        fs::write(root.join(&secret_path), b"clean content").expect("write secret-bearing path");
        git_ok(&root, &["add", &secret_path]);
        let findings = scan_staged(&root).expect("scan secret-bearing path");
        assert!(
            findings
                .iter()
                .any(|finding| finding.source == "staged-path:<path>")
        );
        assert!(!format_findings(&findings).contains(&secret));
        fs::remove_dir_all(&root).expect("remove fixture repository");
    }

    #[test]
    fn worktree_scan_allows_deleted_tracked_files_and_rejects_untracked_keys() {
        let root = temp_repo("worktree-delete");
        git_ok(&root, &["init", "--quiet"]);
        git_ok(&root, &["config", "user.name", "Ferric Test"]);
        git_ok(&root, &["config", "user.email", "ferric@example.invalid"]);
        fs::write(root.join("tracked.txt"), b"clean").expect("write tracked fixture");
        git_ok(&root, &["add", "tracked.txt"]);
        git_ok(
            &root,
            &["commit", "--quiet", "--no-verify", "-m", "tracked fixture"],
        );
        fs::remove_file(root.join("tracked.txt")).expect("delete tracked fixture");
        assert!(
            scan_worktree(&root)
                .expect("scan tracked deletion")
                .is_empty()
        );

        let secret = format!("{}{}", ["AI", "za"].concat(), "Q".repeat(35));
        fs::write(root.join("untracked.txt"), &secret).expect("write untracked key");
        let findings = scan_worktree(&root).expect("scan untracked key");
        assert_eq!(findings.len(), 1);
        assert!(!format_findings(&findings).contains(&secret));
        fs::remove_dir_all(&root).expect("remove fixture repository");
    }

    #[test]
    fn commit_and_annotated_tag_messages_are_scanned() {
        let root = temp_repo("metadata");
        git_ok(&root, &["init", "--quiet"]);
        git_ok(&root, &["config", "user.name", "Ferric Test"]);
        git_ok(&root, &["config", "user.email", "ferric@example.invalid"]);
        fs::write(root.join("clean.txt"), b"clean").expect("write clean fixture");
        git_ok(&root, &["add", "clean.txt"]);

        let commit_secret = format!("{}{}", ["AI", "za"].concat(), "G".repeat(35));
        git_ok_owned(
            &root,
            &[
                "commit".to_owned(),
                "--quiet".to_owned(),
                "--no-verify".to_owned(),
                "-m".to_owned(),
                commit_secret.clone(),
            ],
        );
        let tag_secret = format!("{}{}", ["gh", "p_"].concat(), "H".repeat(36));
        git_ok_owned(
            &root,
            &[
                "tag".to_owned(),
                "-a".to_owned(),
                "metadata-test".to_owned(),
                "-m".to_owned(),
                tag_secret.clone(),
            ],
        );
        let findings = scan_history(&root).expect("scan commit and tag metadata");
        let diagnostic = format_findings(&findings);
        assert!(findings.len() >= 2);
        assert!(!diagnostic.contains(&commit_secret));
        assert!(!diagnostic.contains(&tag_secret));
        fs::remove_dir_all(&root).expect("remove fixture repository");
    }

    #[test]
    fn git_replace_refs_cannot_hide_outgoing_secret() {
        let root = temp_repo("replace-ref");
        git_ok(&root, &["init", "--quiet"]);
        git_ok(&root, &["config", "user.name", "Ferric Test"]);
        git_ok(&root, &["config", "user.email", "ferric@example.invalid"]);
        let secret = format!("{}{}", ["AI", "za"].concat(), "J".repeat(35));
        fs::write(root.join("payload.txt"), &secret).expect("write tainted payload");
        git_ok(&root, &["add", "payload.txt"]);
        git_ok(
            &root,
            &["commit", "--quiet", "--no-verify", "-m", "tainted"],
        );
        let tainted = git_text(&root, &["rev-parse", "HEAD"]).expect("resolve tainted commit");
        fs::write(root.join("payload.txt"), b"clean").expect("write replacement payload");
        git_ok(&root, &["add", "payload.txt"]);
        git_ok(&root, &["commit", "--quiet", "--no-verify", "-m", "clean"]);
        let clean = git_text(&root, &["rev-parse", "HEAD"]).expect("resolve clean commit");
        git_ok(&root, &["replace", tainted.trim(), clean.trim()]);
        install_test_hooks(&root);
        let zero_object_id = "0".repeat(tainted.trim().len());
        let update = format!(
            "refs/heads/topic {} refs/heads/topic {zero_object_id}\n",
            tainted.trim()
        );
        let findings = scan_pre_push(&root, "origin", &update).expect("scan replaced commit");
        assert!(!findings.is_empty());
        assert!(!format_findings(&findings).contains(&secret));
        fs::remove_dir_all(&root).expect("remove fixture repository");
    }

    #[test]
    fn sha256_object_ids_are_accepted() {
        assert!(validate_object_id(&"a".repeat(40)).is_ok());
        assert!(validate_object_id(&"b".repeat(64)).is_ok());
        assert!(is_zero_object_id(&"0".repeat(40)));
        assert!(is_zero_object_id(&"0".repeat(64)));
    }

    #[test]
    fn hook_configuration_requires_exact_content_and_executable_index_mode() {
        let root = temp_repo("hook-integrity");
        git_ok(&root, &["init", "--quiet"]);
        let hooks = root.join(HOOKS_PATH);
        fs::create_dir_all(&hooks).expect("create hook directory");
        fs::write(hooks.join("pre-commit"), PRE_COMMIT_HOOK).expect("write pre-commit");
        fs::write(hooks.join("pre-push"), PRE_PUSH_HOOK).expect("write pre-push");
        git_ok(&root, &["add", "build/git-hooks/pre-commit"]);
        git_ok(&root, &["add", "build/git-hooks/pre-push"]);
        git_ok(
            &root,
            &[
                "update-index",
                "--chmod=+x",
                "build/git-hooks/pre-commit",
                "build/git-hooks/pre-push",
            ],
        );
        git_ok(&root, &["config", "--local", "core.hooksPath", HOOKS_PATH]);
        verify_hook_configuration(&root).expect("verify exact hook configuration");

        git_ok(&root, &["config", "extensions.worktreeConfig", "true"]);
        git_ok(
            &root,
            &["config", "--worktree", "core.hooksPath", "alternate-hooks"],
        );
        let effective_config_error =
            verify_hook_configuration(&root).expect_err("reject effective hook override");
        assert!(effective_config_error.contains("FF-SECRET-E-HOOKS-PATH"));
        git_ok(
            &root,
            &["config", "--worktree", "--unset", "core.hooksPath"],
        );

        fs::write(hooks.join("pre-commit"), b"#!/bin/sh\nexit 0\n").expect("mutate pre-commit");
        let content_error =
            verify_hook_configuration(&root).expect_err("reject altered hook content");
        assert!(content_error.contains("FF-SECRET-E-HOOK-CONTENT"));

        fs::write(hooks.join("pre-commit"), PRE_COMMIT_HOOK).expect("restore pre-commit");
        fs::write(hooks.join("pre-push"), b"#!/bin/sh\nexit 0\n").expect("mutate staged pre-push");
        git_ok(&root, &["add", "build/git-hooks/pre-push"]);
        fs::write(hooks.join("pre-push"), PRE_PUSH_HOOK).expect("restore worktree pre-push");
        let index_content_error =
            verify_hook_configuration(&root).expect_err("reject altered staged hook content");
        assert!(index_content_error.contains("FF-SECRET-E-HOOK-INDEX-CONTENT"));
        git_ok(&root, &["add", "build/git-hooks/pre-push"]);

        git_ok(
            &root,
            &["update-index", "--chmod=-x", "build/git-hooks/pre-commit"],
        );
        let mode_error =
            verify_hook_configuration(&root).expect_err("reject non-executable hook mode");
        assert!(mode_error.contains("FF-SECRET-E-HOOK-MODE"));
        fs::remove_dir_all(&root).expect("remove fixture repository");
    }

    #[test]
    fn new_branch_pre_push_scans_complete_ancestry_and_deletion_is_legal() {
        let root = temp_repo("pre-push");
        git_ok(&root, &["init", "--quiet"]);
        git_ok(&root, &["config", "user.name", "Ferric Test"]);
        git_ok(&root, &["config", "user.email", "ferric@example.invalid"]);

        fs::write(root.join("clean.txt"), b"clean").expect("write clean fixture");
        git_ok(&root, &["add", "clean.txt"]);
        git_ok(&root, &["commit", "--quiet", "--no-verify", "-m", "clean"]);
        let secret = format!("{}{}", ["AI", "za"].concat(), "D".repeat(35));
        fs::write(root.join("credential.txt"), &secret).expect("write secret fixture");
        git_ok(&root, &["add", "credential.txt"]);
        git_ok(&root, &["commit", "--quiet", "--no-verify", "-m", "secret"]);
        install_test_hooks(&root);
        let head = git_text(&root, &["rev-parse", "HEAD"]).expect("resolve head");
        let zero_object_id = "0".repeat(head.trim().len());
        let update = format!(
            "refs/heads/topic {} refs/heads/topic {zero_object_id}\n",
            head.trim(),
        );
        let findings = scan_pre_push(&root, "origin", &update).expect("scan new branch");
        assert_eq!(findings.len(), 1);
        assert!(!format_findings(&findings).contains(&secret));

        let deletion = format!(
            "(delete) {zero_object_id} refs/heads/topic {}\n",
            head.trim(),
        );
        assert!(
            scan_pre_push(&root, "origin", &deletion)
                .expect("scan deletion")
                .is_empty()
        );
        fs::remove_dir_all(&root).expect("remove fixture repository");
    }

    #[test]
    fn tree_root_and_ref_names_are_scanned_before_push() {
        let root = temp_repo("tree-ref");
        git_ok(&root, &["init", "--quiet"]);
        git_ok(&root, &["config", "user.name", "Ferric Test"]);
        git_ok(&root, &["config", "user.email", "ferric@example.invalid"]);
        let secret = format!("{}{}", ["AI", "za"].concat(), "K".repeat(35));
        fs::write(root.join("payload.txt"), &secret).expect("write tree payload");
        git_ok(&root, &["add", "payload.txt"]);
        git_ok(
            &root,
            &["commit", "--quiet", "--no-verify", "-m", "tree payload"],
        );
        install_test_hooks(&root);
        let tree = git_text(&root, &["rev-parse", "HEAD^{tree}"]).expect("resolve tree");
        let zero_object_id = "0".repeat(tree.trim().len());
        let tree_update = format!(
            "refs/tags/tree-root {} refs/tags/tree-root {zero_object_id}\n",
            tree.trim()
        );
        let tree_findings = scan_pre_push(&root, "origin", &tree_update).expect("scan tree root");
        assert!(!tree_findings.is_empty());
        assert!(!format_findings(&tree_findings).contains(&secret));

        let ref_secret = format!("{}{}", ["AI", "za"].concat(), "L".repeat(35));
        let clean = git_text(&root, &["rev-parse", "HEAD"]).expect("resolve clean object");
        let ref_update = format!(
            "refs/heads/{ref_secret} {} refs/heads/{ref_secret} {zero_object_id}\n",
            clean.trim()
        );
        let ref_findings = scan_pre_push(&root, "origin", &ref_update).expect("scan ref names");
        assert!(
            ref_findings
                .iter()
                .any(|finding| finding.source == "pre-push-local-ref:<ref>")
        );
        assert!(!format_findings(&ref_findings).contains(&ref_secret));
        fs::remove_dir_all(&root).expect("remove fixture repository");
    }

    #[test]
    fn history_scans_ref_names_and_non_commit_roots() {
        let root = temp_repo("history-ref");
        git_ok(&root, &["init", "--quiet"]);
        git_ok(&root, &["config", "user.name", "Ferric Test"]);
        git_ok(&root, &["config", "user.email", "ferric@example.invalid"]);
        fs::write(root.join("clean.txt"), b"clean").expect("write clean fixture");
        git_ok(&root, &["add", "clean.txt"]);
        git_ok(&root, &["commit", "--quiet", "--no-verify", "-m", "clean"]);
        let head = git_text(&root, &["rev-parse", "HEAD"]).expect("resolve head");
        let ref_secret = format!("{}{}", ["AI", "za"].concat(), "R".repeat(35));
        git_ok(
            &root,
            &[
                "update-ref",
                &format!("refs/heads/{ref_secret}"),
                head.trim(),
            ],
        );

        fs::write(
            root.join("tree-secret.txt"),
            format!("{}{}", ["AI", "za"].concat(), "S".repeat(35)),
        )
        .expect("write tree secret fixture");
        git_ok(&root, &["add", "tree-secret.txt"]);
        let tree = git_text(&root, &["write-tree"]).expect("write tree");
        git_ok(&root, &["update-ref", "refs/tags/direct-tree", tree.trim()]);

        let findings = scan_history(&root).expect("scan all ref names and object roots");
        assert!(
            findings
                .iter()
                .any(|finding| finding.source == "history-ref:<ref>")
        );
        assert!(findings.iter().any(|finding| {
            finding.source.starts_with("history-object:") && finding.pattern_id == "google-api-key"
        }));
        assert!(!format_findings(&findings).contains(&ref_secret));
        fs::remove_dir_all(&root).expect("remove fixture repository");
    }

    fn install_test_hooks(root: &Path) {
        let hooks = root.join(HOOKS_PATH);
        fs::create_dir_all(&hooks).expect("create hook directory");
        fs::write(hooks.join("pre-commit"), PRE_COMMIT_HOOK).expect("write pre-commit");
        fs::write(hooks.join("pre-push"), PRE_PUSH_HOOK).expect("write pre-push");
        git_ok(root, &["add", "build/git-hooks/pre-commit"]);
        git_ok(root, &["add", "build/git-hooks/pre-push"]);
        git_ok(
            root,
            &[
                "update-index",
                "--chmod=+x",
                "build/git-hooks/pre-commit",
                "build/git-hooks/pre-push",
            ],
        );
        git_ok(root, &["config", "--local", "core.hooksPath", HOOKS_PATH]);
    }

    fn temp_repo(label: &str) -> std::path::PathBuf {
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let repository_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("repository root");
        let path = repository_root
            .join(".fforager-artifacts/test-runs/secret-scan")
            .join(format!(
                "fforager-secret-scan-{label}-{}-{epoch}-{sequence}",
                std::process::id()
            ));
        fs::create_dir_all(&path).expect("create fixture repository");
        path
    }

    fn git_ok(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .expect("run git fixture command");
        assert!(status.success(), "git fixture command failed: {args:?}");
    }

    fn git_ok_owned(root: &Path, args: &[String]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .expect("run owned git fixture command");
        assert!(status.success(), "git fixture command failed: {args:?}");
    }
}
