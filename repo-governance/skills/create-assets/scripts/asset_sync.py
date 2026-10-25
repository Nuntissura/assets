"""Before-use updates and scoped, independently validated asset publication.

Python 3.11+, Git. No shell execution, stash, reset, force-push, or branch deletion.
"""
import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path, PurePosixPath

REPOSITORY = "https://github.com/Nuntissura/assets"
SKILL = "repo-governance/skills/create-assets"
STATE = ".asset-installation.json"


def run(root, *args, env=None, data=None):
    result = subprocess.run(["git", "-C", str(root), "--literal-pathspecs", *args], input=data, capture_output=True, env=env)
    if result.returncode:
        raise ValueError(result.stderr.decode("utf-8", "replace").strip())
    return result.stdout


def identity(value):
    return value.strip().replace("git@github.com:", "https://github.com/").removesuffix(".git").rstrip("/")


def safe_path(value):
    p = PurePosixPath(value)
    if not value or p.is_absolute() or any(x in p.parts for x in ("..", ".git")) or ":" in value or "\\" in value:
        raise ValueError(f"unsafe path: {value}")
    return p


def sha(data):
    return hashlib.sha256(data.replace(b"\r\n", b"\n")).hexdigest()


def origin(root, expected=REPOSITORY):
    actual = run(root, "remote", "get-url", "origin").decode().strip()
    if identity(actual) != identity(expected):
        raise ValueError("origin identity mismatch; no synchronization performed")
    if run(root, "ls-files", "-u").strip():
        raise ValueError("unresolved Git conflicts; no synchronization performed")
    for name in ("MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD", "rebase-merge", "rebase-apply"):
        value = run(root, "rev-parse", "--git-path", name).decode().strip()
        path = Path(value) if Path(value).is_absolute() else root / value
        if path.exists():
            raise ValueError(f"Git operation in progress: {name}")
    run(root, "fetch", "--prune", "origin", "+refs/heads/*:refs/remotes/origin/*")
    lines = run(root, "ls-remote", "--symref", "origin", "HEAD").decode().splitlines()
    branch = next((line.split()[1].removeprefix("refs/heads/") for line in lines if line.startswith("ref: ")), None)
    if not branch:
        raise ValueError("origin did not advertise its default branch")
    ref = "refs/remotes/origin/" + branch
    revision = run(root, "rev-parse", ref).decode().strip()
    return branch, ref, revision


def skill_snapshot(root, revision):
    names = run(root, "ls-tree", "-r", "--name-only", revision, "--", SKILL).decode().splitlines()
    result = {}
    for name in names:
        relative = name.removeprefix(SKILL + "/")
        safe_path(relative)
        result[relative] = run(root, "show", revision + ":" + name)
    if "SKILL.md" not in result or "references/assets-index.yaml" not in result:
        raise ValueError("origin has no complete create-assets skill and inventory")
    if result["references/assets-index.yaml"].replace(b"\r\n", b"\n") != run(root, "show", revision + ":assets-index.yaml").replace(b"\r\n", b"\n"):
        raise ValueError("origin skill inventory does not match its canonical catalog")
    return result


def installed_state(root, destination, incoming):
    state_file = destination / STATE
    existing = {p.relative_to(destination).as_posix(): p for p in destination.rglob("*")
                if p.is_file() and "__pycache__" not in p.parts and p.name != STATE} if destination.exists() else {}
    if state_file.is_file():
        state = json.loads(state_file.read_text(encoding="utf-8"))
    else:
        observed = {name: sha(path.read_bytes()) for name, path in existing.items()}
        current = {name: sha(data) for name, data in incoming.items()}
        if not observed or observed == current:
            state = {"files": observed}
        else:
            # A restored mirror may contain an older official skill but no device-local installation state.
            history = run(root, "log", "--all", "--format=%H", "--", SKILL).decode().splitlines()
            state = None
            for revision in history:
                try:
                    snapshot = skill_snapshot(root, revision)
                except ValueError:
                    continue
                candidate = {name: sha(data) for name, data in snapshot.items()}
                if candidate == observed:
                    state = {"files": candidate}
                    break
            if state is None:
                raise ValueError("local skill edits preserved; installed files match no recorded repository version")
    baseline, pending = state["files"], state.get("pending", {})
    for name in set(baseline) | set(pending):
        safe_path(name)
    conflicts = []
    for name, path in existing.items():
        allowed = {baseline.get(name), pending.get(name)} - {None}
        if sha(path.read_bytes()) not in allowed:
            conflicts.append(name)
    for name in baseline:
        if name not in existing and name not in pending:
            conflicts.append(name + " (locally deleted)")
    if conflicts:
        raise ValueError("local skill edits preserved; conflicts: " + ", ".join(sorted(conflicts)))
    return state


def install_snapshot(destination, incoming, state, revision):
    destination.mkdir(parents=True, exist_ok=True)
    pending = {name: sha(data) for name, data in incoming.items()}
    state_file = destination / STATE
    state_file.write_text(json.dumps({"files": state["files"], "pending": pending,
                                     "revision": revision}, indent=2), encoding="utf-8")
    for name, content in incoming.items():
        target = destination / safe_path(name)
        if not target.resolve().is_relative_to(destination.resolve()):
            raise ValueError(f"skill path escapes install directory: {name}")
        target.parent.mkdir(parents=True, exist_ok=True)
        temporary = target.with_name(target.name + ".asset-update")
        temporary.write_bytes(content)
        os.replace(temporary, target)
    for name in set(state["files"]) - set(incoming):
        path = destination / safe_path(name)
        if path.exists():
            path.unlink()
    state_file.write_text(json.dumps({"files": pending, "revision": revision}, indent=2), encoding="utf-8")


def advance_checkout(root, ref, head, revision):
    receipt = root / ".local/asset-publication/latest.json"
    state = json.loads(receipt.read_text(encoding="utf-8")) if receipt.is_file() else {}
    if state.get("base") != head or state.get("revision") != revision:
        run(root, "merge", "--ff-only", "--no-edit", ref)
        return
    paths = state["paths"]
    for path in paths:
        safe_path(path)
    # Adopt our own already-published files through a separate index, retaining other staged work.
    index_name = run(root, "rev-parse", "--git-path", "index").decode().strip()
    real_index = Path(index_name) if Path(index_name).is_absolute() else root / index_name
    lock_path = real_index.with_name(real_index.name + ".lock")
    with lock_path.open("x") as lock:
        try:
            temporary = receipt.parent / ("adopt-" + revision + ".index")
            shutil.copyfile(real_index, temporary)
            env = dict(os.environ, GIT_INDEX_FILE=str(temporary))
            staged = run(root, "diff", "--cached", "--name-only", "-z", head, "--", *paths).decode().split("\0")
            for path in filter(None, staged):
                if run(root, "diff", "--cached", "--name-only", revision, "--", path).strip():
                    raise ValueError(f"new staged work conflicts with our publication: {path}")
            run(root, "add", "-A", "--", *paths, env=env)
            if run(root, "diff", "--cached", "--name-only", revision, "--", *paths, env=env).strip():
                raise ValueError("new local edits conflict with our publication; all work preserved")
            run(root, "merge", "--ff-only", "--no-edit", ref, env=env)
            os.replace(temporary, real_index)
        finally:
            lock.close()
            lock_path.unlink()


def before_use(root, destinations, expected=REPOSITORY):
    branch, ref, revision = origin(root, expected)
    incoming = skill_snapshot(root, revision)
    head = run(root, "rev-parse", "HEAD").decode().strip()
    repository_skill = (root / SKILL).resolve()
    catalog_content = (root / "assets-index.yaml").read_bytes()
    published_catalog = run(root, "show", revision + ":assets-index.yaml")
    baseline_catalog = run(root, "show", head + ":assets-index.yaml")
    staged_catalog = bool(run(root, "diff", "--cached", "--name-only", head, "--", "assets-index.yaml").strip())
    local_catalog = staged_catalog or sha(catalog_content) != sha(baseline_catalog)
    if local_catalog:
        generated = repository_skill / "references/assets-index.yaml"
        if not generated.is_file() or sha(generated.read_bytes()) != sha(catalog_content):
            raise ValueError("local generated inventory mismatch; canonical catalog and skill copy preserved")
        if sha(published_catalog) != sha(baseline_catalog):
            receipt = root / ".local/asset-publication/latest.json"
            publication = json.loads(receipt.read_text(encoding="utf-8")) if receipt.is_file() else {}
            own_publication = (publication.get("base") == head and publication.get("revision") == revision
                               and {"assets-index.yaml", SKILL + "/references/assets-index.yaml"}
                               .issubset(publication.get("paths", []))
                               and sha(catalog_content) == sha(published_catalog))
            if not own_publication:
                raise ValueError("incoming catalog update conflicts with unpublished local catalog; all work preserved")
            # advance_checkout must still verify every receipt path against the published tree and index.
    staged_controls = run(root, "diff", "--cached", "--name-only", "-z", head, "--", SKILL).decode().split("\0")
    for path in filter(None, staged_controls):
        if local_catalog and path == SKILL + "/references/assets-index.yaml":
            indexed_inventory = run(root, "show", ":" + path)
            indexed_catalog = run(root, "show", ":assets-index.yaml")
            if sha(indexed_inventory) not in {sha(indexed_catalog), sha(catalog_content)}:
                raise ValueError("staged generated inventory mismatch; index and working catalogs preserved")
            continue
        raise ValueError(f"staged repository skill change preserved: {path}")
    baseline_skill = skill_snapshot(root, head)
    repository_files = {path.relative_to(repository_skill).as_posix(): path for path in repository_skill.rglob("*")
                        if path.is_file() and "__pycache__" not in path.parts and path.name != STATE}
    for name in sorted(set(baseline_skill) | set(repository_files)):
        if local_catalog and name == "references/assets-index.yaml":
            continue
        local = repository_files.get(name)
        if local is None or name not in baseline_skill or sha(local.read_bytes()) != sha(baseline_skill[name]):
            raise ValueError(f"local repository skill change preserved: {name}")
    # The repository inventory is a generated source projection, not an installed skill customization.
    # Git advances its official controls; only external copies receive the fetched installation.
    states = [None if destination.resolve() == repository_skill else installed_state(root, destination, incoming)
              for destination in destinations]
    if head != revision:
        result = subprocess.run(["git", "-C", str(root), "merge-base", "--is-ancestor", head, ref], capture_output=True)
        if result.returncode:
            raise ValueError(f"local commits diverge from origin; preserved HEAD={head}, origin={revision}")
        # Git itself rejects incoming changes that would overwrite staged, unstaged, or untracked work.
        advance_checkout(root, ref, head, revision)
    if run(root, "rev-parse", "HEAD").decode().strip() != revision:
        raise ValueError("checkout did not reach the fetched revision")
    local_catalog = (sha((root / "assets-index.yaml").read_bytes()) != sha(published_catalog)
                     or bool(run(root, "diff", "--cached", "--name-only", revision, "--", "assets-index.yaml").strip()))
    # Control-file edits are conflicts even when no branch advancement was necessary.
    for name, content in incoming.items():
        if local_catalog and name == "references/assets-index.yaml":
            continue
        local = root / SKILL / safe_path(name)
        if not local.is_file() or sha(local.read_bytes()) != sha(content):
            raise ValueError(f"local repository skill change preserved: {name}")
    for destination, state in zip(destinations, states):
        if state is not None:
            install_snapshot(destination, incoming, state, revision)
    return {"status": "UPDATED", "origin_revision": revision, "branch": branch,
            "reread_skill": [str(p / "SKILL.md") for p in destinations],
            "local_catalog": "UNPUBLISHED" if local_catalog else "CURRENT",
            "source_synchronization": "NOT_SYNCHRONIZED" if local_catalog else "NOT_VERIFIED",
            "local_work": "preserved; repository catalog and generated inventory remain unpublished" if local_catalog
                          else "preserved; only a fast-forward and managed skill installation were allowed"}


def publish(root, plan, message, expected=REPOSITORY):
    branch, ref, revision = origin(root, expected)
    if run(root, "rev-parse", "HEAD").decode().strip() != revision:
        raise ValueError("run before-use and reconcile local commits before publishing")
    # The inventory is read through the validated catalog implementation, not a second parser.
    import importlib.util
    module_path = root / "repo-governance/scripts/asset_catalog.py"
    spec = importlib.util.spec_from_file_location("publication_catalog", module_path)
    catalog = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(catalog)
    index = catalog.yaml.safe_load((root / "assets-index.yaml").read_text(encoding="utf-8"))
    assets = {a["id"]: a for a in index["assets"]}
    scopes, selected_paths, executable_paths, checks = [], [], [], []
    for item in plan["assets"]:
        asset = assets[item["id"]]
        scope = str(safe_path(asset["path"]))
        scope_parts = PurePosixPath(scope).parts
        if (not scope_parts or scope_parts[0].startswith(".")
                or scope_parts[0] in {"repo-governance", "source-downloads", "node_modules", "target"}
                or not item.get("checks") or not item.get("paths")):
            raise ValueError(f"asset scope or required proof missing: {item['id']}")
        scopes.append(scope)
        item_paths = []
        for value in item["paths"]:
            path = str(safe_path(value))
            if not path.startswith(scope + "/") or (root / path).is_dir():
                raise ValueError("publication paths must name task-owned files inside their asset scope")
            if not (root / path).is_file():
                existing = run(root, "ls-tree", "-z", "HEAD", "--", path)
                if not existing or existing.split(b"\t", 1)[0].split()[1] != b"blob":
                    raise ValueError(f"publication requires an exact existing or tracked deleted file: {path}")
            selected_paths.append(path)
            item_paths.append(path)
        for value in item.get("executable_paths", []):
            path = str(safe_path(value))
            if path not in item_paths or not (root / path).is_file():
                raise ValueError("executable paths must name selected files inside their asset scope")
            executable_paths.append(path)
        for check in item["checks"]:
            cwd = str(safe_path(check.get("cwd", scope)))
            if cwd != scope and not cwd.startswith(scope + "/"):
                raise ValueError("proof working directory escapes its asset scope")
            if not isinstance(check.get("argv"), list) or not check["argv"] or not check.get("authority"):
                raise ValueError("each proof requires argv and an inspected authority/manual reference")
            safe_path(check["authority"])
            checks.append((cwd, check))
    if not scopes:
        raise ValueError("publication requires at least one asset")
    previous = catalog.yaml.safe_load(run(root, "show", revision + ":assets-index.yaml"))
    previous_assets = {a["id"]: a for a in previous["assets"]}
    selected = {a["id"] for a in plan["assets"]}
    for asset_id in set(assets) | set(previous_assets):
        if assets.get(asset_id) != previous_assets.get(asset_id) and asset_id not in selected:
            raise ValueError(f"unrelated catalog edit preserved: {asset_id}")
    for key in set(index) | set(previous):
        if key not in ("assets", "updated_at") and index.get(key) != previous.get(key):
            raise ValueError(f"catalog control change requires separate maintenance: {key}")
    # A private index preserves the user's real index, including staged work in the selected asset.
    area = root / ".local" / "asset-publication"
    area.mkdir(parents=True, exist_ok=True)
    session = Path(tempfile.mkdtemp(prefix="candidate-", dir=area))
    env = dict(os.environ, GIT_INDEX_FILE=str(session / "index"))
    run(root, "read-tree", "HEAD", env=env)
    allowed = selected_paths + ["assets-index.yaml", SKILL + "/references/assets-index.yaml"]
    run(root, "add", "-A", "--", *allowed, env=env)
    if executable_paths:
        run(root, "update-index", "--chmod=+x", "--", *executable_paths, env=env)
    paths = run(root, "diff", "--cached", "--name-only", "-z", env=env).decode().split("\0")
    if not any(paths):
        raise ValueError("no scoped changes to publish")
    tree = run(root, "write-tree", env=env).decode().strip()
    candidate = run(root, "commit-tree", tree, "-p", revision, data=(message + "\n").encode()).decode().strip()
    checkout = session / "proof"
    subprocess.run(["git", "clone", "--shared", "--no-checkout", str(root), str(checkout)],
                   check=True, capture_output=True)
    run(checkout, "config", "core.longpaths", "true")
    run(checkout, "checkout", "--detach", candidate)
    run(checkout, "remote", "set-url", "origin", run(root, "remote", "get-url", "origin").decode().strip())
    result = subprocess.run([sys.executable, str(checkout / "repo-governance/scripts/asset_catalog.py"),
                             "check", "--repo", str(checkout)], capture_output=True, text=True)
    if result.returncode:
        raise ValueError(f"candidate catalog failed; preserved at {checkout}\n{result.stdout}\n{result.stderr}")
    for cwd, check in checks:
        authority = checkout / check["authority"]
        if not authority.is_file():
            raise ValueError(f"proof authority missing from candidate: {check['authority']}")
        result = subprocess.run(check["argv"], cwd=checkout / cwd, capture_output=True, text=True)
        if result.returncode:
            raise ValueError(f"asset proof failed; candidate preserved at {checkout}\n{result.stdout}\n{result.stderr}")
    if run(checkout, "status", "--porcelain").strip():
        raise ValueError(f"proof modified candidate source; preserved at {checkout}")
    # A concurrent remote writer makes the ordinary push reject; never retry by force or overwrite.
    _, _, current = origin(root, expected)
    if current != revision:
        raise ValueError(f"origin advanced during proof: {revision} -> {current}; candidate preserved at {checkout}")
    run(root, "push", "origin", candidate + ":refs/heads/" + branch)
    _, _, observed = origin(root, expected)
    if observed != candidate:
        raise ValueError(f"publication requires reconciliation: pushed {candidate}, observed {observed}")
    if run(root, "rev-parse", observed + "^{tree}").decode().strip() != tree:
        raise ValueError("published tree did not match the validated candidate")
    (area / "latest.json").write_text(json.dumps({"base": revision, "revision": candidate,
                                                 "paths": list(filter(None, paths))}, indent=2), encoding="utf-8")
    return {"status": "PUBLISHED", "revision": candidate, "tree": tree, "paths": list(filter(None, paths)),
            "proof_checkout": str(checkout),
            "local_work": "real index and working tree unchanged; reconcile the new origin revision before further work"}


def discover_repo(value):
    if value:
        return Path(value).resolve()
    configured = os.environ.get("ASSETS_REPO")
    if not configured:
        result = subprocess.run(["git", "config", "--global", "--get", "assets.repo"], capture_output=True, text=True)
        configured = result.stdout.strip()
    if configured:
        return Path(configured).resolve()
    result = subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True)
    if result.returncode:
        raise ValueError("no asset checkout configured; provide --repo or ASSETS_REPO")
    return Path(result.stdout.strip()).resolve()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("before-use", "publish"))
    parser.add_argument("--repo")
    parser.add_argument("--skill-dir", action="append", type=Path)
    parser.add_argument("--plan", type=Path)
    parser.add_argument("--message", default="Update validated reusable assets")
    args = parser.parse_args()
    try:
        root = discover_repo(args.repo)
        if args.command == "before-use":
            destinations = args.skill_dir or [Path(__file__).resolve().parents[1]]
            result = before_use(root, destinations)
        else:
            if not args.plan:
                raise ValueError("publish requires --plan with asset IDs and authority-derived proof commands")
            result = publish(root, json.loads(args.plan.read_text(encoding="utf-8")), args.message)
        print(json.dumps(result, indent=2))
        return 0
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError) as exc:
        print(json.dumps({"status": "STOPPED", "reason": str(exc)}, indent=2))
        return 1


if __name__ == "__main__":
    sys.exit(main())
