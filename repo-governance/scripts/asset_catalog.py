"""Catalog discovery, generated skill inventory, and read-only GitHub parity checks.

Python 3.11+ and PyYAML. Run --help from any directory; paths resolve from --repo.
Checks never merge, stage, commit, push, or overwrite installed skills.
"""
import argparse
import hashlib
import json
import subprocess
import sys
import tomllib
from pathlib import Path, PurePosixPath

import yaml

INDEX = "assets-index.yaml"
SKILL = "repo-governance/skills/create-assets"
MANIFESTS = {"Cargo.toml", "package.json", "pyproject.toml"}
SKIP = {".git", ".local", "target", "node_modules", "__pycache__"}
INSTALL_STATE = ".asset-installation.json"


def git(root, *args):
    result = subprocess.run(["git", "-C", str(root), *args], capture_output=True)
    if result.returncode:
        raise ValueError(result.stderr.decode("utf-8", "replace").strip())
    return result.stdout


def normalized(data):
    return data.replace(b"\r\n", b"\n")


def digest(data):
    return hashlib.sha256(normalized(data)).hexdigest()


def relative(path):
    p = PurePosixPath(path)
    if not path or p.is_absolute() or ".." in p.parts or "\\" in path or ":" in path:
        raise ValueError(f"unsafe repository path: {path}")
    return p


class Tree:
    def __init__(self, root, ref=None):
        self.root, self.ref = root, ref
        self.contents = {}
        if ref:
            listing = git(root, "ls-tree", "-r", "-z", ref).split(b"\0")
            self.objects = {}
            for row in filter(None, listing):
                meta, path = row.split(b"\t", 1)
                mode, kind, oid = meta.decode().split()
                self.objects[path.decode()] = (mode, kind, oid)
            self.files = set(self.objects)
        else:
            self.objects = {}
            self.files = set(filter(None, git(root, "ls-files", "--cached", "--others",
                                              "--exclude-standard", "-z").decode().split("\0")))
            self.files = {p for p in self.files if (root / p).is_file()}

    def read(self, path):
        relative(path)
        if path not in self.files:
            raise ValueError(f"missing {self.ref or 'working-tree'}:{path}")
        if self.ref:
            if path not in self.contents:
                self.contents[path] = git(self.root, "cat-file", "blob", self.objects[path][2])
            return self.contents[path]
        resolved = (self.root / path).resolve()
        if not resolved.is_relative_to(self.root.resolve()):
            raise ValueError(f"path escapes repository: {path}")
        return resolved.read_bytes()

    def package(self, path):
        text = self.read(path).decode("utf-8-sig")
        folder = str(PurePosixPath(path).parent)
        if path.endswith("Cargo.toml"):
            data = tomllib.loads(text)
            info = data.get("package")
            if not info:
                return None
            version = info.get("version")
            workspace = {}
            if isinstance(version, dict):
                for parent in PurePosixPath(path).parents:
                    candidate = str(parent / "Cargo.toml")
                    if candidate in self.files:
                        workspace = tomllib.loads(self.read(candidate).decode("utf-8-sig")).get("workspace", {})
                        if "package" in workspace:
                            break
                version = workspace.get("package", {}).get("version")
            dependencies = {}
            tables = [data.get("dependencies", {}), data.get("build-dependencies", {}),
                      data.get("dev-dependencies", {})]
            for target in data.get("target", {}).values():
                tables.extend(target.get(k, {}) for k in ("dependencies", "build-dependencies", "dev-dependencies"))
            for table in tables:
                for name, spec in table.items():
                    if isinstance(spec, dict) and spec.get("workspace"):
                        spec = workspace.get("dependencies", {}).get(name, spec)
                    dependencies[name] = spec.get("package", name) if isinstance(spec, dict) else name
            api = str(PurePosixPath(folder) / data.get("lib", {}).get("path", "src/lib.rs"))
            return {"name": info["name"], "manifest": path, "version": version,
                    "api": api if api in self.files else path,
                    "dependency_names": sorted(set(dependencies.values()))}
        if path.endswith("package.json"):
            data = json.loads(text)
            if not data.get("name"):
                return None
            return {"name": data["name"], "manifest": path, "version": data.get("version"),
                    "api": path, "dependency_names": sorted(data.get("dependencies", {}))}
        data = tomllib.loads(text).get("project", {})
        if not data.get("name"):
            return None
        return {"name": data["name"], "manifest": path, "version": data.get("version"),
                "api": path, "dependency_names": []}


def excluded(path, index):
    for previous, current in index.get("legacy_root_paths", {}).items():
        if path == previous or path.startswith(previous + "/"):
            path = current + path[len(previous):]
            break
    return any(part in SKIP for part in PurePosixPath(path).parts) or any(
        path == item["path"] or path.startswith(item["path"].rstrip("/") + "/")
        for item in index.get("exclusions", []))


def packages(tree, index):
    return {p: item for p in sorted(tree.files)
            if PurePosixPath(p).name in MANIFESTS and not excluded(p, index)
            if (item := tree.package(p)) is not None}


def source_tree(root, index, entry, cache):
    branch = entry.get("source_branch", index["repository"]["branch"])
    if branch == index["repository"]["branch"]:
        if "local" not in cache:
            cache["local"] = Tree(root)
        return cache["local"]
    ref = "refs/remotes/origin/" + branch
    if ref not in cache:
        cache[ref] = Tree(root, ref)
    return cache[ref]


def projection(index_bytes):
    return normalized(index_bytes)


def refresh(root):
    path = root / INDEX
    index = yaml.safe_load(path.read_text(encoding="utf-8"))
    cache = {}
    names = {p["name"] for a in index["assets"] for p in a.get("packages", [])}
    for entry in index["assets"]:
        tree = source_tree(root, index, entry, cache)
        if tree.ref:
            entry["source_tree"] = git(root, "rev-parse", tree.ref + ":" + entry["path"]).decode().strip()
        for package in entry.get("packages", []):
            actual = tree.package(package["manifest"])
            if actual is None:
                raise ValueError(f"not a package: {package['manifest']}")
            dependencies = actual.pop("dependency_names")
            package.update(actual)
            package["depends_on"] = sorted(n for n in dependencies if n in names)
    encoded = yaml.safe_dump(index, sort_keys=False, allow_unicode=True, width=110).encode()
    path.write_bytes(encoded)
    snapshot = root / SKILL / "references/assets-index.yaml"
    snapshot.parent.mkdir(parents=True, exist_ok=True)
    snapshot.write_bytes(projection(encoded))


def check(root, index=None, tree=None):
    tree = tree or Tree(root)
    index = index or yaml.safe_load(tree.read(INDEX))
    errors = []
    seen_ids, seen_paths, owned, entries = set(), set(), {}, index.get("assets", [])
    cache = {"local": tree}
    actual_packages = packages(tree, index)
    if index.get("coverage") != "complete":
        errors.append("catalog coverage is not complete")
    if index.get("schema") != "assets.index.v1":
        errors.append("unsupported catalog schema")
    for entry in entries:
        try:
            for key in ("id", "name", "path", "kind", "purpose", "capabilities", "status"):
                if not entry.get(key):
                    errors.append(f"{entry.get('id')}: missing {key}")
            relative(entry["path"])
            asset_root = index.get("repository", {}).get("asset_root")
            if asset_root and not entry["path"].startswith(asset_root + "/"):
                errors.append(f"{entry['id']}: asset is outside the assets parent folder")
            if entry["id"] in seen_ids:
                errors.append(f"duplicate ID {entry['id']}")
            if entry["path"] in seen_paths:
                errors.append(f"duplicate path {entry['path']}")
            seen_ids.add(entry["id"])
            seen_paths.add(entry["path"])
            source = source_tree(root, index, entry, cache)
            if source.ref and entry.get("source_branch"):
                observed = git(root, "rev-parse", source.ref + ":" + entry["path"]).decode().strip()
                if entry.get("source_tree") != observed:
                    errors.append(f"{entry['id']}: remote source-tree drift; inspect changes before refresh")
            if not any(p.startswith(entry["path"] + "/") for p in source.files):
                errors.append(f"{entry['id']}: folder absent in source")
            for key in ("manual", "contract"):
                if entry.get(key):
                    source.read(entry[key])
            for package in entry.get("packages", []):
                manifest = package["manifest"]
                if manifest in owned:
                    errors.append(f"duplicate package ownership {manifest}")
                owned[manifest] = entry["id"]
                actual = source.package(manifest)
                if actual is None:
                    errors.append(f"not a package {manifest}")
                    continue
                for key in ("name", "version", "api"):
                    if package.get(key) != actual[key]:
                        errors.append(f"{manifest}: {key} drift, catalog={package.get(key)!r}, source={actual[key]!r}")
                source.read(package["api"])
            if entry.get("fork_of") and not entry.get("fork_decision"):
                errors.append(f"{entry['id']}: fork has no approved decision path")
            if entry.get("fork_decision"):
                source.read(entry["fork_decision"])
        except (ValueError, KeyError, TypeError) as exc:
            errors.append(str(exc))
    for entry in entries:
        for child in entry.get("components", []):
            if child not in seen_ids:
                errors.append(f"{entry['id']}: unknown component {child}")
    for manifest in actual_packages:
        if manifest not in owned:
            errors.append(f"uncatalogued package {manifest}")
    for item in index.get("drafts", []):
        relative(item["path"])
    expected = projection(tree.read(INDEX))
    try:
        if normalized(tree.read(SKILL + "/references/assets-index.yaml")) != expected:
            errors.append("generated skill index is stale; run refresh")
    except ValueError as exc:
        errors.append(str(exc))
    return errors


def skill_files(root):
    folder = root / SKILL
    return {p.relative_to(folder).as_posix(): p.read_bytes() for p in folder.rglob("*")
            if p.is_file() and "__pycache__" not in p.parts and p.name != INSTALL_STATE}


def install(root, destination):
    errors = check(root)
    if errors:
        raise ValueError("\n".join(errors))
    for name, content in skill_files(root).items():
        path = destination / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)


def parity(root, skill_dirs):
    index = yaml.safe_load((root / INDEX).read_text(encoding="utf-8"))
    findings = []
    def finding(code, target, detail):
        findings.append({"code": code, "target": str(target), "detail": detail})
    url = git(root, "remote", "get-url", "origin").decode().strip().removesuffix(".git").rstrip("/")
    if url != index["repository"]["url"].removesuffix(".git").rstrip("/"):
        raise ValueError("origin does not match the catalog repository identity")
    # Fetch all branch tips without rewriting the worktree or force-updating local branches.
    git(root, "fetch", "--prune", "origin", "+refs/heads/*:refs/remotes/origin/*")
    for error in check(root):
        finding("CATALOG_DRIFT", INDEX, error)
    branch = index["repository"]["branch"]
    head = git(root, "ls-remote", "--symref", "origin", "HEAD").decode().splitlines()
    default = next((line.split()[1].removeprefix("refs/heads/") for line in head
                    if line.startswith("ref: ")), None)
    if default is None:
        raise ValueError("origin HEAD did not identify a canonical branch; fresh parity unavailable")
    if default != branch:
        finding("ORIGIN_DEFAULT_BRANCH_DRIFT", branch, f"GitHub default branch is {default}; reconcile catalog authority")
    ref = "refs/remotes/origin/" + branch
    remote = Tree(root, ref)
    sha = git(root, "rev-parse", ref).decode().strip()
    try:
        for error in check(root, tree=remote):
            finding("ORIGIN_CATALOG_DRIFT", sha, error)
    except (ValueError, KeyError) as exc:
        finding("ORIGIN_CATALOG_DRIFT", sha, str(exc))
    if git(root, "rev-parse", "HEAD").decode().strip() != sha:
        finding("REVISION_DIVERGENCE", "HEAD", f"canonical origin revision is {sha}; inspect before integrating")
    local = Tree(root)
    changed = set(filter(None, git(root, "diff", "--name-only", "-z", ref, "--").decode().split("\0")))
    for path in sorted(local.files | remote.files):
        if excluded(path, index):
            continue
        if path not in remote.files:
            finding("LOCAL_ONLY", path, "not published on canonical branch")
        elif path not in local.files:
            finding("ORIGIN_ONLY", path, "not present in local working tree")
        elif path in changed:
            finding("CONTENT_DRIFT", path, "working tree differs from freshly fetched origin source")
    # All branch manifests are discovery inputs, including assets never merged to the default branch.
    known = {p["manifest"] for a in index["assets"] for p in a.get("packages", [])}
    refs = git(root, "for-each-ref", "--format=%(refname)", "refs/remotes/origin/").decode().splitlines()
    for candidate in refs:
        if candidate.endswith("/HEAD"):
            continue
        candidate_tree = Tree(root, candidate)
        for path in packages(candidate_tree, index):
            relocated = index.get("legacy_source_paths", {}).get(path, path)
            if relocated not in known:
                finding("UNINDEXED_ORIGIN_PACKAGE", candidate, path)
    for entry in index["assets"]:
        entry_branch = entry.get("source_branch", branch)
        source = Tree(root, "refs/remotes/origin/" + entry_branch)
        for package in entry.get("packages", []):
            try:
                actual = source.package(package["manifest"])
                if not actual or any(actual[k] != package.get(k) for k in ("name", "version")):
                    finding("ORIGIN_VERSION_DRIFT", package["manifest"], f"source branch {entry_branch}")
            except ValueError as exc:
                finding("ORIGIN_VERSION_DRIFT", package["manifest"], str(exc))
    expected_files = skill_files(root)
    for directory in skill_dirs:
        for name, content in expected_files.items():
            path = directory / name
            if not path.is_file() or digest(path.read_bytes()) != digest(content):
                finding("SKILL_DRIFT", path, "installed or mirrored copy differs from repository skill")
        if directory.is_dir():
            for path in directory.rglob("*"):
                if path.is_file() and "__pycache__" not in path.parts and path.name != INSTALL_STATE:
                    name = path.relative_to(directory).as_posix()
                    if name not in expected_files:
                        finding("SKILL_EXTRA_FILE", path, "not present in the repository-owned skill")
    source_revisions = {name.removeprefix("refs/remotes/origin/"):
                        git(root, "rev-parse", name).decode().strip() for name in refs
                        if not name.endswith("/HEAD")}
    return {"status": "IN_SYNC" if not findings else "DRIFT", "origin_revision": sha,
            "source_branch": branch, "checked_skill_directories": [str(p) for p in skill_dirs],
            "origin_branch_revisions": source_revisions, "findings": findings}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("check", "refresh", "install", "parity", "search", "discover"))
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--skill-dir", type=Path, action="append", default=[])
    parser.add_argument("--query", default="")
    args = parser.parse_args()
    root = args.repo.resolve()
    try:
        if args.command == "refresh":
            refresh(root)
            errors = check(root)
        elif args.command == "check":
            errors = check(root)
        elif args.command == "install":
            if not args.skill_dir:
                raise ValueError("install requires an explicit --skill-dir")
            for directory in args.skill_dir:
                install(root, directory)
            errors = []
        elif args.command == "parity":
            if not args.skill_dir:
                raise ValueError("parity requires at least one explicit --skill-dir; check every installed/mirror copy")
            result = parity(root, args.skill_dir)
            print(json.dumps(result, indent=2))
            return 0 if result["status"] == "IN_SYNC" else 1
        else:
            index = yaml.safe_load((root / INDEX).read_text(encoding="utf-8"))
            if args.command == "discover":
                result = list(packages(Tree(root), index).values())
            else:
                words = args.query.casefold().split()
                result = [a for a in index["assets"] if all(w in json.dumps(a).casefold() for w in words)]
            print(json.dumps(result, indent=2))
            return 0
        print(json.dumps({"status": "FAIL" if errors else "PASS", "errors": errors}, indent=2))
        return bool(errors)
    except (ValueError, KeyError, OSError, tomllib.TOMLDecodeError, yaml.YAMLError) as exc:
        print(json.dumps({"status": "ERROR", "error": str(exc)}))
        return 2


if __name__ == "__main__":
    sys.exit(main())
