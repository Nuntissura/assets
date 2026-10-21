//! Source-backed incompatible watcher; the production source and binary stay intact.

use super::*;

const BINARY: &str = "fforager-watcher-incompatible";

#[derive(Debug)]
struct Lease {
    path: PathBuf,
    file: Option<File>,
    released: bool,
}

impl Lease {
    fn release(&mut self) -> Result<(), String> {
        drop(self.file.take());
        fs::remove_file(&self.path).map_err(failure)?;
        self.released = true;
        Ok(())
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        drop(self.file.take());
        if !self.released
            && let Err(error) = fs::remove_file(&self.path)
        {
            eprintln!("watcher variant lease release failed: {error}");
        }
    }
}

fn refresh(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if fs::read(path).ok().as_deref() != Some(bytes) {
        fs::write(path, bytes).map_err(failure)?;
    }
    Ok(())
}

fn lock_identities(value: &toml::Value) -> Result<BTreeSet<String>, String> {
    value
        .get("package")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| failure("lock package inventory absent"))?
        .iter()
        .map(|package| {
            let identity = serde_json::json!({
                "name": package.get("name").and_then(toml::Value::as_str),
                "version": package.get("version").and_then(toml::Value::as_str),
                "source": package.get("source").and_then(toml::Value::as_str),
                "checksum": package.get("checksum").and_then(toml::Value::as_str)
            });
            if identity["name"].is_null() || identity["version"].is_null() {
                return Err(failure("lock package identity missing"));
            }
            Ok(identity.to_string())
        })
        .collect()
}

pub(super) fn build(root: &Path, stage: &Path) -> Result<PathBuf, String> {
    let artifacts = root.join(".fforager-artifacts");
    let lease_path = artifacts.join("watcher-incompatible-source.lock");
    let lease_file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lease_path)
        .map_err(|error| failure(format!("watcher variant source ownership: {error}")))?;
    let mut lease = Lease {
        path: lease_path,
        file: Some(lease_file),
        released: false,
    };
    let source = artifacts.join("watcher-incompatible-source");
    prepare_sources(root, &source, stage)?;
    let manifest_path = slash(&source.join("Cargo.toml"));
    let target = slash(&artifacts.join("cargo-target"));
    command_output_with_timeout(
        root,
        "cargo",
        &[
            "build",
            "--offline",
            "--locked",
            "--release",
            "--manifest-path",
            &manifest_path,
            "--bin",
            BINARY,
            "--target-dir",
            &target,
        ],
        TOOL_COMMAND_TIMEOUT,
    )?;
    let binary = artifacts
        .join("cargo-target/release")
        .join(executable(BINARY));
    validate_built_runtime_artifact(root, &binary)?;
    let digest = sha256_file(&binary)?;
    let staged = stage.join(executable(BINARY));
    fs::copy(&binary, &staged).map_err(failure)?;
    if sha256_file(&staged)? != digest {
        return Err(failure("watcher variant changed during staging"));
    }
    fs::write(stage.join("watcher-mutant-artifact.sha256"), digest).map_err(failure)?;
    lease.release()?;
    Ok(staged)
}

fn variant_manifest(root: &Path) -> Result<String, String> {
    let workspace: toml::Value =
        toml::from_str(&fs::read_to_string(root.join("build/Cargo.toml")).map_err(failure)?)
            .map_err(failure)?;
    let mut manifest: toml::Value = toml::from_str(
        &fs::read_to_string(root.join("product/watcher/Cargo.toml")).map_err(failure)?,
    )
    .map_err(failure)?;
    let package = manifest
        .get_mut("package")
        .and_then(toml::Value::as_table_mut)
        .ok_or_else(|| failure("watcher package absent"))?;
    package.remove("workspace");
    package.insert("autobins".into(), false.into());
    manifest["bin"][0]["name"] = BINARY.into();
    manifest["dependencies"]["fforager-diagnostics-contract"]["path"] =
        slash(&root.join("product/crates/fforager-diagnostics-contract")).into();
    let canonical_workspace = workspace
        .get("workspace")
        .and_then(toml::Value::as_table)
        .ok_or_else(|| failure("canonical workspace absent"))?;
    let mut isolated = toml::map::Map::new();
    isolated.insert(
        "resolver".into(),
        canonical_workspace
            .get("resolver")
            .ok_or("workspace resolver absent")?
            .clone(),
    );
    for key in ["package", "dependencies", "lints"] {
        isolated.insert(
            key.into(),
            canonical_workspace
                .get(key)
                .ok_or("workspace inheritance absent")?
                .clone(),
        );
    }
    let manifest_table = manifest
        .as_table_mut()
        .ok_or("watcher manifest is not a table")?;
    manifest_table.insert("workspace".into(), toml::Value::Table(isolated));
    manifest_table.insert(
        "profile".into(),
        workspace
            .get("profile")
            .ok_or("canonical profiles absent")?
            .clone(),
    );
    toml::to_string(&manifest).map_err(failure)
}

fn prepare_lock(root: &Path, source: &Path, stage: &Path, manifest_text: &str) -> Result<Vec<u8>, String> {
    let canonical_lock = fs::read(root.join("build/Cargo.lock")).map_err(failure)?;
    let canonical_lock_digest = sha256_bytes(&canonical_lock);
    let resolve = fs::read(source.join("Cargo.toml")).ok().as_deref()
        != Some(manifest_text.as_bytes())
        || fs::read(source.join("canonical-lock.sha256"))
            .ok()
            .as_deref()
            != Some(canonical_lock_digest.as_bytes())
        || !source.join("Cargo.lock").is_file();
    refresh(&source.join("Cargo.toml"), manifest_text.as_bytes())?;
    let manifest_path = slash(&source.join("Cargo.toml"));
    if resolve {
        // Preserve approved resolutions instead of selecting newer cached packages.
        refresh(&source.join("Cargo.lock"), &canonical_lock)?;
    }
    // Full resolution is required: --no-deps leaves a seeded workspace lock untouched.
    command_output_with_timeout(
            root,
            "cargo",
            &[
                "metadata",
                "--offline",
                "--format-version",
                "1",
                "--manifest-path",
                &manifest_path,
            ],
            TOOL_COMMAND_TIMEOUT,
        )?;
    let lock_bytes = fs::read(source.join("Cargo.lock")).map_err(failure)?;
    let resolved: toml::Value =
        toml::from_str(std::str::from_utf8(&lock_bytes).map_err(failure)?).map_err(failure)?;
    let canonical: toml::Value =
        toml::from_str(std::str::from_utf8(&canonical_lock).map_err(failure)?).map_err(failure)?;
    if !lock_identities(&resolved)?.is_subset(&lock_identities(&canonical)?) {
        return Err(failure(
            "watcher variant resolved an unapproved package identity",
        ));
    }
    refresh(
        &source.join("canonical-lock.sha256"),
        canonical_lock_digest.as_bytes(),
    )?;
    fs::write(stage.join("watcher-mutant-Cargo.toml"), manifest_text).map_err(failure)?;
    fs::write(stage.join("watcher-mutant-Cargo.lock"), &lock_bytes).map_err(failure)?;
    Ok(lock_bytes)
}

fn prepare_sources(root: &Path, source: &Path, stage: &Path) -> Result<(), String> {
    fs::create_dir_all(source.join("src")).map_err(failure)?;
    for entry in fs::read_dir(source).map_err(failure)? {
        let name = entry.map_err(failure)?.file_name();
        if !["src", "Cargo.toml", "Cargo.lock", "canonical-lock.sha256"]
            .iter()
            .any(|allowed| name == *allowed)
        {
            return Err(failure("unexpected warm watcher variant workspace entry"));
        }
    }
    let original = fs::read(root.join("product/watcher/src/main.rs")).map_err(failure)?;
    let text = std::str::from_utf8(&original).map_err(failure)?;
    if text.matches("ff.watcher-ready@1").count() != 1 {
        return Err(failure(
            "watcher source must contain exactly one ready schema literal",
        ));
    }
    let source_entries = fs::read_dir(root.join("product/watcher/src"))
        .map_err(failure)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(failure)?;
    if source_entries.len() != 1 || source_entries[0].file_name() != "main.rs" {
        return Err(failure(
            "watcher source closure changed; variant copy requires review",
        ));
    }
    for entry in fs::read_dir(source.join("src")).map_err(failure)? {
        let entry = entry.map_err(failure)?;
        if entry.file_name() != "main.rs" {
            return Err(failure("unexpected warm watcher variant source entry"));
        }
    }
    let mutated = text.replacen("ff.watcher-ready@1", "ff.watcher-ready@9", 1);
    refresh(&source.join("src/main.rs"), mutated.as_bytes())?;
    let manifest_text = variant_manifest(root)?;
    let lock_bytes = prepare_lock(root, source, stage, &manifest_text)?;
    let identity = serde_json::json!({
        "schema":"ff.watcher-source-variant@1", "source_commit":command_output(root,"git", &["rev-parse","HEAD"])?.trim(),
        "source_tree":command_output(root,"git", &["rev-parse","HEAD^{tree}"])?.trim(),
        "original_source_sha256":sha256_bytes(&original), "mutated_source_sha256":sha256_bytes(mutated.as_bytes()),
        "manifest_sha256":sha256_bytes(manifest_text.as_bytes()), "lock_sha256":sha256_bytes(&lock_bytes),
        "mutation":"exactly one ready schema ff.watcher-ready@1 -> ff.watcher-ready@9", "binary":BINARY
    });
    fs::write(
        stage.join("watcher-mutant-build.json"),
        serde_json::to_vec_pretty(&identity).map_err(failure)?,
    )
    .map_err(failure)?;
    fs::write(stage.join("watcher-original-source.rs"), &original).map_err(failure)?;
    fs::write(stage.join("watcher-mutated-source.rs"), mutated).map_err(failure)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_variant_uses_approved_lock_and_emits_incompatible_wire() {
        let root = repo_root().expect("repository root");
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_nanos();
        let stage = root.join(".fforager-artifacts/runtime-proof")
            .join(format!("watcher-variant-component-{}-{stamp}", std::process::id()));
        fs::create_dir_all(&stage).expect("owned component stage");
        let binary = build(&root, &stage).expect("actual approved-dependency variant build");
        let ready = stage.join("ready.json");
        let stop = stage.join("stop");
        fs::write(&stop, b"stop\n").expect("governed stop before startup");
        let stdout = stage.join("watcher.stdout");
        let stderr = stage.join("watcher.stderr");
        let mut command = Command::new(&binary);
        command.current_dir(&stage)
            .arg("--state-dir").arg(stage.join("diagnostics"))
            .arg("--worker-pid").arg(std::process::id().to_string())
            .arg("--ready-file").arg(&ready)
            .arg("--stop-file").arg(&stop)
            .stdin(Stdio::null())
            .stdout(Stdio::from(File::create(&stdout).expect("component stdout")))
            .stderr(Stdio::from(File::create(&stderr).expect("component stderr")));
        configure_quiet_process(&mut command);
        let child = command.spawn().expect("real incompatible watcher");
        let watcher_pid = child.id();
        // Reuse the existing bounded owned-child cleanup guard, without creating
        // a production-invocation receipt for this component-only watcher run.
        let mut owned = Invocation { child, stdout, stderr, started: Instant::now() };
        let status = wait_for_child(&mut owned.child, "component-incompatible-watcher", &[], Duration::from_secs(5))
            .expect("bounded watcher stop and reap");
        assert!(status.success(), "actual watcher stderr: {}", bounded_capture(&owned.stderr).expect("stderr"));
        let value: Value = serde_json::from_slice(&fs::read(&ready).expect("physical ready message"))
            .expect("actual ready JSON");
        assert_eq!(value["schema"], "ff.watcher-ready@9");
        assert_eq!(value["worker_pid"].as_u64(), Some(u64::from(std::process::id())));
        assert_eq!(value["watcher_pid"].as_u64(), Some(u64::from(watcher_pid)));
        assert_eq!(value["ready"], true);
    }
}
