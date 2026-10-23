"""Populate the Prism oracle corpus (<workspace corpus root>/<corpus.json icc_subdir>) from installed system profiles.

Reads `t1_lcms2.json`: every profile with origin "installed" names a file, its SHA-256 and the system
directories it was found in (`default_locations`). This script only READS those directories (plus any
`--from DIR`), copies files whose SHA-256 matches into the corpus root, and writes `MANIFEST.json` there.
It never builds or runs tests. Exit 1 when a profile could not be provided (the tests then report it as
NOT_PROVEN). The corpus root follows the workspace convention `.config/oracle-corpus.toml` (start at the
workspace root, walk up at most `max-ancestors` parents, first ancestor A where A/<search> is a directory),
the same walk the tests do; `--root DIR` names the ICC directory explicitly (e.g. before the root exists).
Usage: python tests/oracle/populate_corpus.py [--from DIR]... [--root DIR]
"""
import argparse
import hashlib
import json
import os
import shutil
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
CRATE = os.path.dirname(os.path.dirname(HERE))
WORKSPACE = os.path.dirname(os.path.dirname(CRATE))


def corpus_root():
    """The workspace oracle corpus root per .config/oracle-corpus.toml, or None when no ancestor holds it."""
    values = {}
    with open(os.path.join(WORKSPACE, ".config", "oracle-corpus.toml"), encoding="utf-8") as f:
        for line in f:
            if "=" in line and not line.lstrip().startswith("#"):
                k, v = line.split("=", 1)
                values[k.strip()] = v.strip().strip('"')
    at = WORKSPACE
    for _ in range(int(values["max-ancestors"]) + 1):
        if os.path.isdir(os.path.join(at, values["search"])):
            return os.path.join(at, values["search"])
        parent = os.path.dirname(at)
        if parent == at:
            break
        at = parent
    return None


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--from", dest="sources", action="append", default=[], help="extra directory to read profiles from")
    ap.add_argument("--root", help="ICC directory (default: <workspace corpus root>/<corpus.json icc_subdir>)")
    args = ap.parse_args()
    corpus = json.load(open(os.path.join(HERE, "corpus.json"), encoding="utf-8"))
    fixture = json.load(open(os.path.join(HERE, "t1_lcms2.json"), encoding="utf-8"))
    if args.root:
        root = os.path.normpath(args.root)
    else:
        base = corpus_root()
        if base is None:
            print("corpus root absent (.config/oracle-corpus.toml walk found no directory); pass --root DIR")
            return 1
        root = os.path.normpath(os.path.join(base, corpus["icc_subdir"]))
    os.makedirs(root, exist_ok=True)
    manifest, missing = [], []
    for key, spec in sorted(fixture["profiles"].items()):
        if spec.get("origin") != "installed":
            continue
        name, want = spec["file"], spec["sha256"]
        target = os.path.join(root, name)
        if os.path.isfile(target) and sha256(target) == want:
            manifest.append({"key": key, "file": name, "sha256": want, "bytes": os.path.getsize(target), "copied_from": "already_present"})
            continue
        found = None
        for d in args.sources + spec.get("default_locations", []):
            cand = os.path.join(d, name)
            if os.path.isfile(cand) and sha256(cand) == want:
                found = cand
                break
        if found is None:
            missing.append(f"{key}: {name} (sha256 {want[:12]}...) not found with matching SHA-256")
            continue
        shutil.copyfile(found, target)
        if sha256(target) != want:
            missing.append(f"{key}: copy of {found} does not verify")
            continue
        manifest.append({"key": key, "file": name, "sha256": want, "bytes": os.path.getsize(target), "copied_from": found})
    with open(os.path.join(root, "MANIFEST.json"), "w", encoding="utf-8", newline="\n") as f:
        json.dump({"schema": "hsk.prism.oracle_corpus_manifest.v1", "root": root, "profiles": manifest, "missing": missing}, f, indent=1)
        f.write("\n")
    print(f"corpus root {root}: {len(manifest)} profiles present, {len(missing)} missing")
    for m in missing:
        print("MISSING " + m)
    return 1 if missing else 0


if __name__ == "__main__":
    sys.exit(main())
