"""Compile the real WASM boundary and compare it with native Rust fixture outputs."""
from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys


def tool(variable: str, name: str) -> str:
    selected = os.environ.get(variable) or shutil.which(name)
    if not selected or not Path(selected).is_file():
        raise SystemExit(f"Missing {name}; configure {variable}")
    return os.path.abspath(selected)


def run(argv: list[str], workspace: Path, **kwargs) -> subprocess.CompletedProcess:
    print(json.dumps({"command": argv}), flush=True)
    try:
        return subprocess.run(argv, cwd=workspace, check=True, **kwargs)
    except subprocess.CalledProcessError as error:
        for captured in (error.stdout, error.stderr):
            if captured:
                print(captured.decode(errors="replace") if isinstance(captured, bytes) else captured, file=sys.stderr, flush=True)
        raise


def main() -> None:
    workspace = Path(__file__).resolve().parents[2]
    bindgen = tool("HSSCRAP_WASM_BINDGEN", "wasm-bindgen")
    node = tool("HSSCRAP_NODE", "node")
    cargo = tool("HSSCRAP_CARGO", "cargo")
    version = subprocess.run([bindgen, "--version"], check=True, capture_output=True, text=True).stdout.strip()
    if version != "wasm-bindgen 0.2.129":
        raise SystemExit("wasm-bindgen must match pinned Rust dependency 0.2.129")
    target = Path(os.environ.get("CARGO_TARGET_DIR", workspace / "target"))
    if not target.is_absolute():
        target = workspace / target
    target = target.resolve()
    output = Path(os.environ.get("HSSCRAP_WASM_OUTPUT", target / "hsscrap-wasm-proof"))
    if not output.is_absolute():
        output = workspace / output
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    fixture_path = output / "native-fixtures.json"
    # Do not ask Cargo to build the native fixture binary for the browser target.
    native = run([cargo, "run", "--locked", "-p", "hsscrap-wasm", "--bin", "hsscrap-wasm-reference"], workspace, capture_output=True, text=True)
    print(native.stderr, end="", flush=True)
    json.loads(native.stdout)
    fixture_path.write_text(native.stdout, encoding="utf-8")
    run([cargo, "build", "--locked", "-p", "hsscrap-wasm", "--lib", "--target", "wasm32-unknown-unknown"], workspace)
    wasm = target / "wasm32-unknown-unknown" / "debug" / "hsscrap_wasm.wasm"
    bindings = output / "bindings"
    run([bindgen, "--target", "web", "--out-dir", str(bindings), "--out-name", "hsscrap_wasm", str(wasm)], workspace)
    (bindings / "package.json").write_text('{"type":"module","private":true}\n', encoding="utf-8")
    run([node, str(Path(__file__).with_name("instantiate.mjs")), str(bindings / "hsscrap_wasm.js"), str(fixture_path)], workspace)
    print(json.dumps({"status": "pass", "wasm": str(wasm), "native_fixtures": str(fixture_path)}), flush=True)


if __name__ == "__main__":
    main()
