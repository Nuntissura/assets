"""Studio workspace registry sync (STUDIO-WORKSPACE steward tool; Python 3.11+, stdlib only).

Derives `[workspace.metadata.studio.registry.<crate>]` (direct + full Cargo.lock closure, kind pure-rust|native-ffi)
and the tools/workspace-probe.rs APPROVED table from member crate manifests + Cargo.lock, so pins are mechanical.

Usage: python -X utf8 -I tools/registry-sync.py <workspace_root> [--write [--requests <dep-requests.jsonl>] [--allow-unrequested]]
Check mode (default, read-only): exit 0 in sync, 1 when root Cargo.toml registry metadata or probe APPROVED drift.
--write: rewrite both; a newly approved (crate, lib) pair needs a matching request line unless --allow-unrequested.
Exit 2 = dependency missing from Cargo.lock or ambiguous lock edge (lock must be derived first).
Crate spec defects (builder-owned manifests) are reported in "spec_defects", not fatal; the probe rejects them per crate.
"""
import json, pathlib, re, sys, tomllib

CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
DEP_SECTIONS = ("dependencies", "dev-dependencies", "build-dependencies")
# Non -sys names the steward labels native-ffi (only valid in native owners; the probe rejects them elsewhere;
# the steward license gate reads this one-line set). cpal: OS audio FFI provider (WASAPI/ALSA/CoreAudio) of
# hsk-studio-score-device feature cpal-backend (lead decision 2026-10-11T01:40Z, native per designated owner).
NATIVE_FFI_EXTRA: set[str] = {"cpal"}


def fail(code, msg):
    print(f"ERROR: {msg}")
    sys.exit(code)


def key(k):
    return k if re.fullmatch(r"[A-Za-z0-9_-]+", k) else json.dumps(k)


def arr(v):
    return "[" + ", ".join(json.dumps(x) for x in v) + "]"


def main():
    args = sys.argv[1:]
    root = pathlib.Path(args[0])
    write, allow = "--write" in args, "--allow-unrequested" in args
    req_path = pathlib.Path(args[args.index("--requests") + 1]) if "--requests" in args else None
    manifest_text = (root / "Cargo.toml").read_text(encoding="utf-8")
    manifest = tomllib.loads(manifest_text)
    lock = tomllib.loads((root / "Cargo.lock").read_text(encoding="utf-8"))
    pk = {(p["name"], p["version"]): p for p in lock["package"]}
    byname = {}
    for n, v in pk:
        byname.setdefault(n, []).append(v)

    def ident(edge):
        f = edge.split()
        if len(f) == 1:
            if len(byname[f[0]]) != 1:
                fail(2, f"ambiguous lock edge {edge}")
            return (f[0], byname[f[0]][0])
        return (f[0], f[1])

    registry = {}  # owner -> {lib: (version, features)}
    defects = []  # crate manifest problems the probe will reject (builder-owned files)
    for member in manifest["workspace"]["members"]:
        crate = tomllib.loads((root / member / "Cargo.toml").read_text(encoding="utf-8"))
        owner = crate["package"]["name"]
        tables = [crate.get(s, {}) for s in DEP_SECTIONS]
        for t in crate.get("target", {}).values():
            tables += [t.get(s, {}) for s in DEP_SECTIONS]
        for table in tables:
            for alias, spec in table.items():
                if isinstance(spec, dict) and "path" in spec:
                    continue
                where = f"{owner}: {alias}"
                if not isinstance(spec, dict):
                    fail(2, f"{where}: use inline table with exact version, default-features = false, features")
                lib = spec.get("package", alias)
                ver = spec.get("version", "")
                if not re.fullmatch(r"=\d+\.\d+\.\d+", ver):
                    defects.append(f"{where}: version must be exact '=X.Y.Z' (got {ver!r}); not pinned")
                    continue
                if spec.get("default-features") is not False:
                    defects.append(f"{where}: default-features = false required; not pinned")
                    continue
                if "features" not in spec:
                    defects.append(f"{where}: features = [...] missing; pinned as [] but probe rejects registry_features_missing")
                if set(spec) - {"version", "features", "default-features", "optional", "package"}:
                    defects.append(f"{where}: unsupported keys {sorted(set(spec))}; probe rejects")
                entry = (ver[1:], sorted(spec.get("features", [])))
                prev = registry.setdefault(owner, {}).get(lib)
                if prev and prev != entry:
                    fail(2, f"{where}: conflicting specs {prev} vs {entry}")
                if (lib, entry[0]) not in pk:
                    fail(2, f"{where}: {lib}@{entry[0]} not in Cargo.lock (run cargo first)")
                registry[owner][lib] = entry

    sections, approved = [], []
    for owner in [m.rsplit("/", 1)[1] for m in manifest["workspace"]["members"]]:
        direct = registry.get(owner)
        if not direct:
            continue
        seen, stack = set(), [(lib, v) for lib, (v, _) in direct.items()]
        while stack:
            i = stack.pop()
            if i not in seen:
                seen.add(i)
                stack.extend(ident(e) for e in pk[i].get("dependencies", []))
        locked = []
        for n, v in sorted(seen):
            p = pk[(n, v)]
            if p.get("source") != CRATES_IO or not p.get("checksum"):
                fail(2, f"{owner}: {n}@{v} is not a crates.io package with checksum")
            kind = "native-ffi" if n.endswith(("-sys", "_sys")) or n in NATIVE_FFI_EXTRA else "pure-rust"
            locked.append(f'"{n}@{v}" = {{ source = "{CRATES_IO}", checksum = "{p["checksum"]}", kind = "{kind}" }}')
        d = ", ".join(
            f'{key(lib)} = {{ version = "{v}", features = {arr(f)}, default-features = false }}'
            for lib, (v, f) in sorted(direct.items())
        )
        sections.append(
            f"[workspace.metadata.studio.registry.{owner}]\ndirect = {{ {d} }}\nlocked = {{ {', '.join(locked)} }}\n"
        )
        approved.append((owner, sorted(direct)))

    # Root manifest: drop all registry sections, append regenerated ones.
    blocks = re.split(r"(?m)^(?=\[)", manifest_text)
    kept = [b for b in blocks if not b.startswith("[workspace.metadata.studio.registry.")]
    new_manifest = "".join(kept).rstrip("\n") + "\n\n" + "\n".join(sections)

    probe_path = root / "tools" / "workspace-probe.rs"
    probe = probe_path.read_text(encoding="utf-8")
    m = re.search(r"// BEGIN GENERATED APPROVED\n(.*?)// END GENERATED APPROVED", probe, re.S)
    if not m:
        fail(2, "probe APPROVED markers missing")
    old_pairs = set(re.findall(r'"(hsk-studio-[^"]+)", &\[([^\]]*)\]', m.group(1)))
    old_set = {(o, lib) for o, libs in old_pairs for lib in re.findall(r'"([^"]+)"', libs)}
    body = "const APPROVED: &[(&str, &[&str])] = &[\n" + "".join(
        f'    ("{o}", &[{", ".join(json.dumps(x) for x in libs)}]),\n' for o, libs in approved
    ) + "];\n"
    new_probe = probe[: m.start(1)] + body + probe[m.end(1):]

    new_set = {(o, lib) for o, libs in approved for lib in libs}
    added, removed = sorted(new_set - old_set), sorted(old_set - new_set)
    requested = set()
    if req_path and req_path.exists():
        for line in req_path.read_text(encoding="utf-8").splitlines():
            if line.strip():
                r = json.loads(line)
                requested.add((r["crate"], r["dep"]))
    unrequested = [a for a in added if a not in requested]
    print(json.dumps({"owners": [o for o, _ in approved], "approved_added": added, "approved_removed": removed,
                      "unrequested": unrequested, "spec_defects": defects, "manifest_changed": new_manifest != manifest_text,
                      "probe_changed": new_probe != probe}))
    if write and unrequested and not allow:
        fail(1, "unrequested new direct libraries; add request lines or pass --allow-unrequested after review")
    if write:
        (root / "Cargo.toml").write_bytes(new_manifest.encode("utf-8"))
        probe_path.write_bytes(new_probe.encode("utf-8"))
    elif new_manifest != manifest_text or new_probe != probe:
        sys.exit(1)


main()
