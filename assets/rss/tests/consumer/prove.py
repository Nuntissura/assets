"""Build external RSS consumers of one exact immutable Git source selection."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
from urllib.parse import parse_qs, urlsplit

REPOSITORY = "https://github.com/Nuntissura/assets"
BASE = {"rss-contracts", "rss-core", "rss-acquisition", "rss-provider-syndication", "hsscrap", "handshake_canonical_json"}
FULL = BASE | {"rss-gui", "rss-wasm", "rss-provider-instagram", "rss-provider-youtube", "rss-provider-tiktok", "rss-provider-pinterest", "hsscrap-gui", "hsscrap-wasm"}
BROWSER = {"rss-contracts", "rss-core", "rss-wasm", "hsscrap", "hsscrap-gui", "hsscrap-wasm", "handshake_canonical_json"}
FORBIDDEN = {"portal-server", "portal-feeds", "portal-db", "surrealdb", "surrealdb-sdk", "reqwest", "axum", "sqlx", "rusqlite"}


def tool(variable: str, name: str) -> str:
    selected = os.environ.get(variable) or shutil.which(name)
    if not selected or not Path(selected).is_file():
        raise SystemExit(f"Missing {name}; configure {variable}")
    return os.path.abspath(selected)


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def manifest_dependency(name: str, revision: str, optional: bool = False) -> str:
    option = ", optional = true" if optional else ""
    return f'{name} = {{ git = "{REPOSITORY}", rev = "{revision}", version = "=0.1.0"{option} }}'


def inspect_metadata(text: str, revision: str, expected: set[str], consumer_name: str) -> dict:
    metadata = json.loads(text)
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    root = metadata["resolve"]["root"]
    if packages[root]["name"] != consumer_name:
        raise SystemExit("Wrong external consumer root")
    active, visiting = set(), set()

    def walk(identifier: str) -> None:
        if identifier in visiting:
            raise SystemExit("Dependency graph contains a cycle")
        if identifier in active:
            return
        visiting.add(identifier)
        for dependency in nodes[identifier]["dependencies"]:
            walk(dependency)
        visiting.remove(identifier)
        active.add(identifier)

    walk(root)
    selections = []
    for identifier in active:
        package = packages[identifier]
        source_text = package["source"]
        if source_text is None and identifier != root:
            raise SystemExit("Unexpected local path dependency")
        if package["name"] in FORBIDDEN or package["name"].startswith("portal-"):
            raise SystemExit(f"Host/database/transport dependency: {package['name']}")
        if not (source_text or "").startswith("git+"):
            continue
        source = urlsplit(source_text.removeprefix("git+"))
        if source.scheme != "https" or source.netloc != "github.com" or source.path != "/Nuntissura/assets" or parse_qs(source.query).get("rev") != [revision] or source.fragment != revision:
            raise SystemExit(f"Wrong immutable source: {package['name']}")
        selections.append({"crate": package["name"], "version": package["version"], "source": source_text, "features": nodes[identifier]["features"]})
    if {selection["crate"] for selection in selections} != expected:
        raise SystemExit(f"Unexpected selected Git crates: {selections}")
    for name in ["rss-contracts", "rss-core", "rss-acquisition"]:
        candidates = [identifier for identifier in active if packages[identifier]["name"] == name]
        if not candidates:
            continue
        closure, pending = set(), candidates
        while pending:
            identifier = pending.pop()
            if identifier not in closure:
                closure.add(identifier)
                pending.extend(nodes[identifier]["dependencies"])
        if any(packages[identifier]["name"].startswith("rss-provider-") or packages[identifier]["name"] in {"rss-gui", "rss-wasm", "egui", "eframe", "wgpu", "hsscrap-gui", "hsscrap-wasm"} for identifier in closure):
            raise SystemExit(f"{name} closure includes a concrete provider or GUI")
    return {"selection": sorted(selections, key=lambda value: value["crate"]), "dependency_count": len(active), "acyclic": True}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--revision", required=True)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if not re.fullmatch(r"[0-9a-f]{40}", args.revision):
        parser.error("revision must be an exact lowercase Git commit")
    asset = Path(__file__).resolve().parents[2]
    repository = asset.parents[1]
    target_setting = os.environ.get("CARGO_TARGET_DIR")
    if not target_setting or not Path(target_setting).is_absolute():
        raise SystemExit("Set absolute CARGO_TARGET_DIR to the coordinated existing target")
    output = (args.output or Path(target_setting) / "rss-release-proof").resolve()
    output.mkdir(parents=True, exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix="consumer-", dir=output))
    artifacts = work / "artifacts"
    artifacts.mkdir()
    cargo, rustc, git = tool("RSS_CARGO", "cargo"), tool("RSS_RUSTC", "rustc"), tool("RSS_GIT", "git")
    bindgen, node = tool("RSS_WASM_BINDGEN", "wasm-bindgen"), tool("RSS_NODE", "node")
    environment = dict(os.environ)
    environment["RUSTC"] = rustc
    environment.setdefault("CARGO_NET_GIT_FETCH_WITH_CLI", "true")
    sequence = 0

    def run(argv: list[str], cwd: Path = work) -> str:
        nonlocal sequence
        sequence += 1
        print(json.dumps({"step": sequence, "command": argv}), flush=True)
        result = subprocess.run(argv, cwd=cwd, env=environment, capture_output=True, text=True, encoding="utf-8", errors="replace")
        prefix = work / f"step-{sequence}"
        prefix.with_suffix(".stdout.log").write_text(result.stdout, encoding="utf-8")
        prefix.with_suffix(".stderr.log").write_text(result.stderr, encoding="utf-8")
        if result.returncode:
            print(result.stdout[-16000:], flush=True)
            print(result.stderr[-16000:], flush=True)
            raise SystemExit(f"Command failed ({result.returncode}); logs at {prefix}")
        return result.stdout

    compiler = run([rustc, "-vV"])
    host = next(line.removeprefix("host: ") for line in compiler.splitlines() if line.startswith("host: "))
    if run([bindgen, "--version"]).strip() != "wasm-bindgen 0.2.129":
        raise SystemExit("wasm-bindgen must match exact dependency 0.2.129")
    native = work / "native"
    (native / "src").mkdir(parents=True)
    for source, destination in [("standalone.rs", "main.rs"), ("browser.rs", "browser.rs")]:
        shutil.copyfile(asset / "examples" / source, native / "src" / destination)
    required = ["rss-contracts", "rss-core", "rss-acquisition", "rss-provider-syndication", "hsscrap"]
    optional = ["rss-gui", "rss-wasm", "rss-provider-instagram", "rss-provider-youtube", "rss-provider-tiktok", "rss-provider-pinterest"]
    dependencies = [manifest_dependency(name, args.revision) for name in required] + [manifest_dependency(name, args.revision, True) for name in optional]
    native_manifest = '[package]\nname="rss-release-consumer"\nversion="0.0.0"\nedition="2021"\npublish=false\n[workspace]\n[features]\ndefault=[]\ngui=["dep:rss-gui","dep:rss-wasm","dep:egui"]\nproviders=["dep:rss-provider-instagram","dep:rss-provider-youtube","dep:rss-provider-tiktok","dep:rss-provider-pinterest"]\n[dependencies]\n'
    native_manifest += "\n".join(dependencies) + '\nserde_json="=1.0.151"\negui={version="=0.33.3",features=["accesskit"],optional=true}\n'
    (native / "Cargo.toml").write_text(native_manifest, encoding="utf-8")
    outputs = []
    for label, features, expected in [("headless", [], BASE), ("native-gui-public-providers", ["gui", "providers"], FULL)]:
        feature_args = ["--features", ",".join(features)] if features else []
        metadata_text = run([cargo, "metadata", "--format-version", "1", "--filter-platform", host, *feature_args], native)
        selected = inspect_metadata(metadata_text, args.revision, expected, "rss-release-consumer")
        build = run([cargo, "build", "--locked", "--target", host, "--message-format=json", *feature_args], native)
        messages = [json.loads(line) for line in build.splitlines() if line.strip()]
        paths = [Path(message["executable"]) for message in messages if message.get("reason") == "compiler-artifact" and message.get("executable") and message.get("target", {}).get("name") == "rss-release-consumer"]
        if len(paths) != 1 or not paths[0].is_file():
            raise SystemExit("Cargo did not identify exactly one consumer executable")
        binary = artifacts / (label + paths[0].suffix)
        shutil.copyfile(paths[0], binary)
        result = json.loads(run([str(binary)]))
        checks = ["source_group_profile", "query_public_api", "job_restart_recovery", "rss_science_provenance"]
        if features:
            checks += ["native_gui_controls", "native_bridge_delegation", "public_provider_api_calls"]
        if result.get("status") != "pass" or result.get("checks") != checks:
            raise SystemExit("Consumer did not prove required public workflows")
        metadata_path = artifacts / f"{label}.metadata.json"
        metadata_path.write_text(metadata_text, encoding="utf-8")
        lock = artifacts / f"{label}.Cargo.lock"
        shutil.copyfile(native / "Cargo.lock", lock)
        outputs.append({"kind": label, "target": host, "features": features, **selected, "runtime": result, "artifact": str(binary), "sha256": digest(binary),
                        "inputs": {str(path): digest(path) for path in [native / "Cargo.toml", native / "src/main.rs", native / "src/browser.rs", lock, metadata_path]}})
    browser = work / "browser"
    (browser / "src").mkdir(parents=True)
    shutil.copyfile(asset / "examples/browser.rs", browser / "src/lib.rs")
    (browser / "Cargo.toml").write_text('[package]\nname="rss-release-browser"\nversion="0.0.0"\nedition="2021"\npublish=false\n[workspace]\n[lib]\ncrate-type=["cdylib"]\n[dependencies]\n' + manifest_dependency("rss-wasm", args.revision) + '\nwasm-bindgen="=0.2.129"\nserde_json="=1.0.151"\n', encoding="utf-8")
    wasm_target = "wasm32-unknown-unknown"
    browser_metadata = run([cargo, "metadata", "--format-version", "1", "--filter-platform", wasm_target], browser)
    selected = inspect_metadata(browser_metadata, args.revision, BROWSER, "rss-release-browser")
    build = run([cargo, "build", "--locked", "--target", wasm_target, "--message-format=json"], browser)
    messages = [json.loads(line) for line in build.splitlines() if line.strip()]
    wasm_paths = [Path(path) for message in messages if message.get("reason") == "compiler-artifact" and message.get("target", {}).get("name") == "rss_release_browser" for path in message["filenames"] if path.endswith(".wasm")]
    if len(wasm_paths) != 1 or not wasm_paths[0].is_file():
        raise SystemExit("Cargo did not identify exactly one compiled consumer WASM")
    wasm = artifacts / "rss-release-browser.wasm"
    shutil.copyfile(wasm_paths[0], wasm)
    bindings = artifacts / "bindings"
    run([bindgen, "--target", "web", "--out-dir", str(bindings), "--out-name", "rss_consumer", str(wasm)])
    (bindings / "package.json").write_text('{"type":"module","private":true}\n', encoding="utf-8")
    browser_result = json.loads(run([node, str(Path(__file__).with_name("instantiate.mjs")), str(bindings / "rss_consumer.js")]))
    if browser_result != {"status": "pass", "checks": ["constructor_errors", "direct_rss_bridge", "exact_revision", "actual_query_effect", "actual_empty_completion", "direct_details_boundary", "direct_compose_query", "actual_revoke"]}:
        raise SystemExit("Actual instantiated WASM did not prove the selected boundary behaviors")
    browser_metadata_path = artifacts / "browser.metadata.json"
    browser_metadata_path.write_text(browser_metadata, encoding="utf-8")
    outputs.append({"kind": "browser", "target": wasm_target, "features": [], **selected, "runtime": browser_result, "artifact": str(wasm), "sha256": digest(wasm),
                    "inputs": {str(path): digest(path) for path in [browser / "Cargo.toml", browser / "Cargo.lock", browser / "src/lib.rs", browser_metadata_path]},
                    "bindings": {str(path): digest(path) for path in sorted(bindings.iterdir()) if path.is_file()}})
    source_paths = ["assets/rss", "assets/hsscrap", "assets/shared/handshake_canonical_json"]
    files = run([git, "ls-tree", "-r", "--name-only", args.revision, "--", *source_paths], repository).splitlines()
    excluded = {".local", "target", "node_modules", ".git", ".env"}
    if not files or any(excluded.intersection(Path(file).parts) or Path(file).suffix.lower() in {".pem", ".key", ".pfx", ".p12"} for file in files):
        raise SystemExit("Canonical source selection contains a generated/private path")
    archive = artifacts / "source-package.tar"
    run([git, "archive", "--format=tar", "--output", str(archive), args.revision, "--", *source_paths], repository)
    inputs = [native / "Cargo.toml", native / "Cargo.lock", native / "src/main.rs", native / "src/browser.rs", browser / "Cargo.toml", browser / "Cargo.lock", browser / "src/lib.rs", Path(__file__), Path(__file__).with_name("instantiate.mjs")]
    report = {"status": "pass", "source_revision": args.revision, "source_repository": REPOSITORY, "compiler": compiler.strip(), "outputs": outputs,
        "source_files": files, "source_archive": {"path": str(archive), "sha256": digest(archive)}, "consumer_inputs": {str(path): digest(path) for path in inputs},
        "availability": "immutable Git source selection; generated binaries and modules are local, not uploaded prebuilt artifacts",
        "unproven": ["live provider acquisition", "connected modes", "canonical host persistence", "Portal/browser adoption", "Desktop adoption", "cooperation"]}
    result = work / "result.json"
    result.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"status": "pass", "source_revision": args.revision, "outputs": [value["kind"] for value in outputs], "result": str(result)}), flush=True)


if __name__ == "__main__":
    main()
