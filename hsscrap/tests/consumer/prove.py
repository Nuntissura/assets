"""Build a separate consumer of an exact published Git source selection."""
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
SELECTED = {"hsscrap", "hsscrap-gui", "hsscrap-wasm", "handshake_canonical_json"}


def tool(variable: str, name: str) -> str:
    value = os.environ.get(variable) or shutil.which(name)
    if not value or not Path(value).is_file():
        raise SystemExit(f"Missing {name}; configure {variable}")
    return os.path.abspath(value)


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


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
    target = Path(target_setting).resolve()
    output = (args.output or target / "hsscrap-release-proof").resolve()
    output.mkdir(parents=True, exist_ok=True)
    consumer = Path(tempfile.mkdtemp(prefix="consumer-", dir=output))
    (consumer / "src").mkdir()
    cargo = tool("HSSCRAP_CARGO", "cargo")
    rustc = tool("HSSCRAP_RUSTC", "rustc")
    git = tool("HSSCRAP_GIT", "git")
    environment = dict(os.environ)
    environment["RUSTC"] = rustc
    environment.setdefault("CARGO_NET_GIT_FETCH_WITH_CLI", "true")
    sequence = 0

    def run(argv: list[str], cwd: Path = consumer) -> str:
        nonlocal sequence
        sequence += 1
        print(json.dumps({"step": sequence, "command": argv}), flush=True)
        result = subprocess.run(argv, cwd=cwd, env=environment, capture_output=True, text=True, encoding="utf-8", errors="replace")
        prefix = consumer / f"step-{sequence}"
        prefix.with_suffix(".stdout.log").write_text(result.stdout, encoding="utf-8")
        prefix.with_suffix(".stderr.log").write_text(result.stderr, encoding="utf-8")
        if result.returncode:
            print(result.stdout[-16000:], flush=True)
            print(result.stderr[-16000:], flush=True)
            raise SystemExit(f"Command failed ({result.returncode}); full logs at {prefix}")
        return result.stdout

    compiler = run([rustc, "-vV"])
    host = next(line.removeprefix("host: ") for line in compiler.splitlines() if line.startswith("host: "))
    dependencies = "\n".join(
        f'{name} = {{ git = "{REPOSITORY}", rev = "{args.revision}", version = "=0.1.0" }}'
        for name in ["hsscrap", "hsscrap-gui", "hsscrap-wasm"]
    )
    (consumer / "Cargo.toml").write_text(
        '[package]\nname = "hsscrap-release-consumer"\nversion = "0.0.0"\nedition = "2021"\npublish = false\n'
        '[workspace]\n[dependencies]\n' + dependencies +
        '\negui = { version = "=0.33.3", features = ["accesskit"] }\nserde_json = "=1.0.151"\n',
        encoding="utf-8",
    )
    shutil.copyfile(asset / "examples/standalone.rs", consumer / "src/main.rs")
    metadata_text = run([cargo, "metadata", "--format-version", "1", "--filter-platform", host])
    metadata = json.loads(metadata_text)
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    git_packages = {package["name"] for package in packages.values() if (package["source"] or "").startswith("git+")}
    if git_packages != SELECTED:
        raise SystemExit(f"Unexpected Git dependency set: {sorted(git_packages)}")
    selections = []
    for package in packages.values():
        if package["source"] is None and package["name"] != "hsscrap-release-consumer":
            raise SystemExit("Consumer unexpectedly resolved another local path package")
        if package["name"] not in SELECTED:
            continue
        source = urlsplit((package["source"] or "").removeprefix("git+"))
        if source.scheme != "https" or source.netloc != "github.com" or source.path != "/Nuntissura/assets" or parse_qs(source.query).get("rev") != [args.revision] or source.fragment != args.revision:
            raise SystemExit(f"Wrong immutable source for {package['name']}")
        selections.append({"crate": package["name"], "version": package["version"], "source": package["source"], "features": nodes[package["id"]]["features"]})
    core = next(package["id"] for package in packages.values() if package["name"] == "hsscrap")
    seen, todo = set(), [core]
    while todo:
        identifier = todo.pop()
        if identifier in seen:
            continue
        seen.add(identifier)
        todo.extend(nodes[identifier]["dependencies"])
    forbidden = {"egui", "eframe", "wgpu", "hsscrap-gui", "hsscrap-wasm", "rss-core", "rss-contracts", "rss-acquisition"}
    if forbidden.intersection(packages[identifier]["name"] for identifier in seen):
        raise SystemExit("Core dependency closure includes GUI or RSS")
    build = run([cargo, "build", "--locked", "--target", host, "--message-format=json"])
    messages = [json.loads(line) for line in build.splitlines() if line.strip()]
    executables = [Path(message["executable"]) for message in messages if message.get("reason") == "compiler-artifact" and message.get("executable") and message.get("target", {}).get("name") == "hsscrap-release-consumer"]
    if len(executables) != 1 or not executables[0].is_file():
        raise SystemExit("Cargo did not identify one actual consumer executable")
    result = json.loads(run([str(executables[0])]))
    expected = ["url_normalization", "native_edit_cancel", "native_gui", "bridge_exact_revision", "bridge_panel", "bridge_revoke"]
    if result.get("status") != "pass" or result.get("checks") != expected:
        raise SystemExit("Consumer did not prove the required public calls")
    files = run([git, "ls-tree", "-r", "--name-only", args.revision, "--", "assets/hsscrap", "assets/shared/handshake_canonical_json"], repository).splitlines()
    excluded = {".local", "target", "node_modules", ".git", ".env"}
    if not files or any(excluded.intersection(Path(file).parts) or Path(file).suffix.lower() in {".pem", ".key", ".pfx", ".p12"} for file in files):
        raise SystemExit("Source selection contains an excluded generated/private path")
    archive = consumer / "source-package.tar"
    run([git, "archive", "--format=tar", "--output", str(archive), args.revision, "--", "assets/hsscrap", "assets/shared/handshake_canonical_json"], repository)
    report = {
        "status": "pass", "source_revision": args.revision, "source_repository": REPOSITORY,
        "selection": selections, "target": host, "compiler": compiler.strip(),
        "core_dependency_count": len(seen), "consumer": result,
        "source_files": files, "excluded_path_check": "passed; content review remains independent",
        "local_artifacts": {str(path): digest(path) for path in [consumer / "Cargo.toml", consumer / "Cargo.lock", consumer / "src/main.rs", executables[0], archive]},
        "availability": "immutable Git source selection; generated artifacts are local, not uploaded prebuilt binaries",
    }
    (consumer / "result.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"status": "pass", "source_revision": args.revision, "target": host, "result": str(consumer / "result.json")}), flush=True)


if __name__ == "__main__":
    main()
