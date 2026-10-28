"""Compile the workspace selector and inspect the committed renderer dependency closure."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import tomllib


def run(argv, cwd):
    result = subprocess.run(argv, cwd=cwd, capture_output=True, text=True, encoding="utf-8")
    if result.returncode:
        print(result.stdout, end="")
        print(result.stderr, end="")
        raise SystemExit(result.returncode)
    return result.stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target-dir", required=True)
    args = parser.parse_args()
    workspace = Path(__file__).resolve().parents[1]
    target = Path(args.target_dir).resolve(strict=True)
    repository = Path(run(["git", "rev-parse", "--show-toplevel"], workspace).strip())
    revision = run(["git", "rev-parse", "HEAD"], repository).strip()
    subtree = workspace.relative_to(repository).as_posix()
    assert subtree == "handshake-studio-assets"
    tree = run(["git", "rev-parse", f"{revision}:{subtree}"], repository).strip()
    manifest = tomllib.loads((workspace / "Cargo.toml").read_text(encoding="utf-8"))
    files = ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"]
    files += [member + "/Cargo.toml" for member in manifest["workspace"]["members"]]
    hashes = {}
    for name in files:
        path = workspace / name
        relative = f"{subtree}/{name}"
        expected = run(["git", "rev-parse", f"{revision}:{relative}"], repository).strip()
        observed = run(["git", "hash-object", f"--path={relative}", str(path)], repository).strip()
        assert observed == expected, f"input differs from candidate Git object: {name}"
        hashes[name] = hashlib.sha256(path.read_bytes()).hexdigest()
    output = Path(tempfile.mkdtemp(prefix="native-memory-selection-", dir=target))
    executable = output / ("workspace-probe.exe" if os.name == "nt" else "workspace-probe")
    run([os.environ["RUSTC"], "--edition=2024", str(workspace / "tools/workspace-probe.rs"),
         "-o", str(executable)], workspace)
    self_test = json.loads(run([str(executable), "--self-test"], workspace))
    assert self_test["status"] == "ok" and self_test["cases"] == 9
    provenance = output / "provenance.toml"
    lines = ["[source]", 'repository = "https://github.com/Nuntissura/assets"',
             f"subtree = {json.dumps(subtree)}", f"revision = {json.dumps(revision)}",
             f"tree = {json.dumps(tree)}", "", "[files]"]
    lines += [f"{json.dumps(name)} = {json.dumps(digest)}" for name, digest in hashes.items()]
    provenance.write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")
    result = json.loads(run([str(executable), "--root", str(workspace), "--provenance",
                             str(provenance), "--revision", revision, "--tree", tree,
                             "--package", "hsk-studio-render-gpu"], workspace))
    assert result["status"] == "accepted" and result["compiled_packages"] is False
    assert result["selected"] == ["hsk-studio-render-gpu"]
    assert {"hsk-studio-render-gpu", "hsk-studio-render-gpu-native-memory"} <= set(result["closure"])
    (output / "selection.json").write_text(json.dumps(result, indent=2) + "\n",
                                           encoding="utf-8", newline="\n")
    print(json.dumps({"workspace_selector": "PASS", "self_test_cases": self_test["cases"],
                      "revision": revision, "tree": tree, "output": str(output)}))


if __name__ == "__main__":
    main()
